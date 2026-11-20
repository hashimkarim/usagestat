//! Incremental per-profile transcript cache. Only changed files are reparsed;
//! HTTP requests share the scan and serialized reports for 30 seconds.
//!
//! On disk the cache is a full snapshot plus a small delta of the files that
//! changed since that snapshot, so an active session rewrites only its own
//! entries instead of the whole cache.
use super::local_usage::{self, LocalUsageEvent};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use usagestat_core::{paths, usage_daily};

// v9 resolves unknown Codex models from later metadata for the same turn.
// 10: rows compact per hour (was per 5h block) for hour-of-day reports.
// 11: worktree projects are labelled with their repository.
// 12: live checkouts resolve subfolders and linked worktrees to the main repo.
// 13: events keep the git branch recorded by Claude Code and Codex.
// 14: Codex ledger records pair with their token_count neighbour across counters.
const VERSION: u32 = 14;
const TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Fingerprint {
    len: u64,
    modified: SystemTime,
}

fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let meta = std::fs::metadata(path)?;
    Ok(Fingerprint {
        len: meta.len(),
        modified: meta.modified()?,
    })
}

#[derive(Serialize, Deserialize)]
struct CachedFile {
    fingerprint: Fingerprint,
    events: Vec<LocalUsageEvent>,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    version: u32,
    #[serde(default)]
    pricing_as_of: String,
    /// Identifies the snapshot a delta was written against. 0: never written.
    #[serde(default)]
    generation: u64,
    files: BTreeMap<PathBuf, CachedFile>,
    #[serde(skip)]
    checked: Option<Instant>,
    #[serde(skip)]
    reports: HashMap<String, String>,
    /// Entries changed or removed since the snapshot on disk.
    #[serde(skip)]
    dirty: std::collections::BTreeSet<PathBuf>,
    #[serde(skip)]
    removed: std::collections::BTreeSet<PathBuf>,
    /// Set when most entries changed at once (format or price migration).
    #[serde(skip)]
    needs_snapshot: bool,
    /// Positions (file, event) of the deduplicated events. Reconciling Codex
    /// mirrors hashes every request key, so it runs once per change, not per report.
    #[serde(skip)]
    unique: Option<Vec<(u32, u32)>>,
}

/// A delta larger than this is folded into a new snapshot.
const DELTA_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Serialize)]
struct DeltaOut<'a> {
    version: u32,
    pricing_as_of: &'a str,
    generation: u64,
    files: BTreeMap<&'a PathBuf, &'a CachedFile>,
    removed: &'a std::collections::BTreeSet<PathBuf>,
}

#[derive(Deserialize)]
struct DeltaIn {
    version: u32,
    pricing_as_of: String,
    generation: u64,
    files: BTreeMap<PathBuf, CachedFile>,
    removed: std::collections::BTreeSet<PathBuf>,
}

fn delta_path(snapshot: &Path) -> PathBuf {
    snapshot.with_extension("delta.json")
}

impl Cache {
    fn refresh(
        &mut self,
        files: Vec<PathBuf>,
        scan: impl FnMut(&Path) -> Result<Vec<LocalUsageEvent>>,
    ) -> Result<bool> {
        let refreshed = self.refresh_files(files, scan);
        if matches!(refreshed, Ok(false)) {
            // Unchanged logs give unchanged reports, except the active block.
            self.reports.retain(|key, _| !key.starts_with("blocks:"));
        } else {
            self.reports.clear();
            self.unique = None;
        }
        refreshed
    }

    /// Deduplicated events across all cached files.
    fn unique_events(&mut self) -> Vec<&LocalUsageEvent> {
        if self.unique.is_none() {
            let all: Vec<_> = self
                .files
                .values()
                .enumerate()
                .flat_map(|(file, cached)| {
                    cached
                        .events
                        .iter()
                        .enumerate()
                        .map(move |(event, row)| ((file as u32, event as u32), row))
                })
                .collect();
            let kept = super::codex_usage::unique(&all.iter().map(|(_, row)| *row).collect::<Vec<_>>());
            // `unique` keeps the input order, so one pass maps rows back to positions.
            let mut kept = kept.into_iter().peekable();
            let positions = all
                .iter()
                .filter(|(_, row)| kept.next_if(|next| std::ptr::eq(*next, *row)).is_some())
                .map(|(position, _)| *position)
                .collect();
            self.unique = Some(positions);
        }
        let files: Vec<_> = self.files.values().collect();
        self.unique
            .as_ref()
            .map(|positions| {
                positions
                    .iter()
                    .map(|&(file, event)| &files[file as usize].events[event as usize])
                    .collect()
            })
            .unwrap_or_default()
    }

    fn refresh_files(
        &mut self,
        files: Vec<PathBuf>,
        mut scan: impl FnMut(&Path) -> Result<Vec<LocalUsageEvent>>,
    ) -> Result<bool> {
        let mut changed = self.version != VERSION;
        let migrate_models = self.version == 8;
        // Read once: a catalog installed mid-refresh leaves this stamp stale,
        // so the next refresh reprices again instead of mixing two catalogs.
        let pricing = super::pricing::version();
        let migrate_prices = self.pricing_as_of != pricing;
        if changed && !migrate_models {
            self.files.clear();
        }
        if changed || migrate_prices {
            self.needs_snapshot = true;
        }
        let files: std::collections::BTreeSet<_> = files.into_iter().collect();
        let gone: Vec<_> = self
            .files
            .keys()
            .filter(|path| !files.contains(*path))
            .cloned()
            .collect();
        for path in gone {
            self.files.remove(&path);
            self.dirty.remove(&path);
            self.removed.insert(path);
            changed = true;
        }
        if migrate_models {
            let mut unsupported = Vec::new();
            for (path, file) in &mut self.files {
                if !super::codex_usage::recover_cached_models(path, &mut file.events)? {
                    unsupported.push(path.clone());
                }
            }
            for path in unsupported {
                self.files.remove(&path);
            }
        }
        if migrate_prices {
            // Any rate may have been added or corrected. An event without a
            // price before or after keeps its unknown cost.
            let needs_reprice = |event: &LocalUsageEvent| {
                event.cost_known || super::pricing::is_priced(&event.model)
            };
            for (path, file) in &mut self.files {
                if !file.events.iter().any(needs_reprice) {
                    continue;
                }
                if file
                    .events
                    .iter()
                    .any(|event| event.codex_identity.is_none() && needs_reprice(event))
                {
                    // Claude cache rows are compacted. Recover request boundaries
                    // before applying tiered prompt prices, never price a day as a request.
                    let stamp = fingerprint(path)?;
                    let events = compact(scan(path)?);
                    file.events = events;
                    file.fingerprint = stamp;
                } else {
                    for event in &mut file.events {
                        if needs_reprice(event) {
                            super::pricing::apply(event);
                        }
                    }
                }
            }
            changed = true;
        }
        // Publish corrected historical counters first. A later normal pass
        // independently handles files appended since the saved fingerprints.
        if (migrate_models || migrate_prices) && self.files.len() == files.len() {
            self.version = VERSION;
            self.pricing_as_of = pricing;
            self.checked = Some(Instant::now());
            return Ok(true);
        }
        for path in files {
            if migrate_models && self.files.contains_key(&path) {
                continue;
            }
            let stamp = fingerprint(&path)?;
            if self
                .files
                .get(&path)
                .is_some_and(|file| file.fingerprint == stamp)
            {
                continue;
            }
            let events = compact(scan(&path)?);
            self.removed.remove(&path);
            self.dirty.insert(path.clone());
            self.files.insert(
                path,
                CachedFile {
                    fingerprint: stamp,
                    events,
                },
            );
            changed = true;
        }
        self.version = VERSION;
        self.pricing_as_of = pricing;
        self.checked = Some(Instant::now());
        Ok(changed)
    }

    /// Writes the changed entries as a delta, or a fresh snapshot when there
    /// is none to build on or the delta has grown large.
    fn persist(&mut self, snapshot: &Path) -> Result<()> {
        use usagestat_core::storage::write_atomic;
        if !self.needs_snapshot && self.generation != 0 {
            let delta = DeltaOut {
                version: self.version,
                pricing_as_of: &self.pricing_as_of,
                generation: self.generation,
                files: self
                    .dirty
                    .iter()
                    .filter_map(|path| self.files.get_key_value(path))
                    .collect(),
                removed: &self.removed,
            };
            let bytes = serde_json::to_vec(&delta)?;
            if bytes.len() <= DELTA_LIMIT {
                write_atomic(&delta_path(snapshot), &bytes)?;
                return Ok(());
            }
        }
        let previous = self.generation;
        self.generation = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(1, |since| since.as_nanos() as u64)
            .max(previous + 1);
        if let Err(error) = serde_json::to_vec(&*self)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| write_atomic(snapshot, &bytes).map_err(Into::into))
        {
            // The old snapshot and its delta are still a consistent pair.
            self.generation = previous;
            return Err(error);
        }
        // A delta left by a crash here names the old generation and is ignored.
        let _ = std::fs::remove_file(delta_path(snapshot));
        self.dirty.clear();
        self.removed.clear();
        self.needs_snapshot = false;
        Ok(())
    }

    /// Reads a snapshot and the delta written against it.
    fn load(snapshot: &Path) -> Self {
        let Some(mut cache) = std::fs::read(snapshot)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok())
            .filter(|cache| cache.version == VERSION)
        else {
            return Self::default();
        };
        let delta = std::fs::read(delta_path(snapshot))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<DeltaIn>(&bytes).ok())
            .filter(|delta| {
                cache.generation != 0
                    && delta.generation == cache.generation
                    && delta.version == cache.version
                    && delta.pricing_as_of == cache.pricing_as_of
            });
        if let Some(delta) = delta {
            for path in &delta.removed {
                cache.files.remove(path);
            }
            // Still unsaved in the snapshot: the next delta must carry them too.
            cache.dirty = delta.files.keys().cloned().collect();
            cache.removed = delta.removed;
            cache.files.extend(delta.files);
        }
        cache
    }

    fn report(&mut self, report: &str) -> Result<String> {
        self.report_limited(report, None)
    }

    fn report_limited(&mut self, report: &str, limit: Option<usize>) -> Result<String> {
        let key = format!("{report}:{limit:?}");
        if let Some(body) = self.reports.get(&key) {
            return Ok(body.clone());
        }
        let body = local_usage::report_limited(self.unique_events().into_iter(), report, limit)?;
        // Bound arbitrary limit variants requested by API clients.
        if self.reports.len() >= 8 {
            self.reports.clear();
        }
        self.reports.insert(key, body.clone());
        Ok(body)
    }
}

// Preserve the dimensions needed by every report, while dropping per-turn rows.
// Price each event BEFORE compaction so long-context rates remain per request.
fn compact(events: Vec<LocalUsageEvent>) -> Vec<LocalUsageEvent> {
    let mut rows: BTreeMap<(String, i64, String, String, String), LocalUsageEvent> =
        BTreeMap::new();
    let mut identified = Vec::new();
    for event in events {
        // Request evidence must survive disk reload and cross-file reconciliation.
        if event.codex_identity.is_some() {
            identified.push(event);
            continue;
        }
        let key = (
            event.ts.format("%Y-%m-%d").to_string(),
            event.ts.timestamp().div_euclid(3600),
            event.session_id.clone(),
            event.model.clone(),
            event.branch.clone(),
        );
        rows.entry(key)
            .and_modify(|row| {
                let mut totals = row.metrics();
                totals.add(&event.metrics());
                row.input_tokens = totals.input_tokens;
                row.output_tokens = totals.output_tokens;
                row.cache_read_tokens = totals.cache_read_tokens;
                row.cache_creation_tokens = totals.cache_creation_tokens;
                row.reasoning_output_tokens = totals.reasoning_output_tokens;
                row.cost_usd = totals.cost_usd;
                row.cost_components = totals.cost_components;
                row.cost_known = totals.cost_known;
                row.cache_savings_usd = totals.cache_savings_usd;
                row.ts = row.ts.max(event.ts);
            })
            .or_insert(event);
    }
    identified.extend(rows.into_values());
    identified
}

pub(super) fn report(provider: &str, report: &str) -> Result<String> {
    report_limited(provider, report, None)
}

pub(super) fn report_limited(provider: &str, report: &str, limit: Option<usize>) -> Result<String> {
    let cache = refreshed(provider)?;
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    cache.report_limited(report, limit)
}

/// Run `f` over the provider's deduplicated events, refreshing from logs first.
pub(super) fn with_events<T>(
    provider: &str,
    f: impl FnOnce(&[&LocalUsageEvent]) -> T,
) -> Result<T> {
    let cache = refreshed(provider)?;
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    Ok(f(&cache.unique_events()))
}

fn refreshed(provider: &str) -> Result<Arc<Mutex<Cache>>> {
    type Key = (PathBuf, String, Vec<PathBuf>);
    type SharedCache = Arc<Mutex<Cache>>;
    static CACHES: OnceLock<Mutex<HashMap<Key, SharedCache>>> = OnceLock::new();
    let Some(source) = local_usage::LocalSource::from_provider_id(provider) else {
        anyhow::bail!("unsupported local provider");
    };
    let roots = source.roots()?;
    let key = (paths::data_dir()?, provider.to_string(), roots.clone());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    let cache_path = key.0.join(format!(
        "local-usage-{provider}-{:016x}.json",
        hasher.finish()
    ));
    let cache = CACHES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(Cache::load(&cache_path))))
        .clone();
    let shared = cache;
    let mut cache = shared.lock().unwrap_or_else(|e| e.into_inner());
    if cache.checked.is_none_or(|at| at.elapsed() >= TTL) {
        let first = cache.checked.is_none();
        let files = roots.iter().flat_map(|root| source.files(root)).collect();
        let changed = cache.refresh(files, |path| {
            let mut events = Vec::new();
            source.scan(path, &mut events)?;
            Ok(events)
        })?;
        if changed {
            // Persist only token/cost metadata, never conversation contents.
            if let Err(error) = cache.persist(&cache_path) {
                log::warn!("could not persist local usage cache: {error}");
            }
        }
        // Saved history changes only with the logs; once per process covers a
        // store that was replaced or pruned while the cache stayed current.
        if changed || first {
            let daily = cache.report("daily")?;
            usage_daily::ingest_json(provider, &daily)?;
        }
        let today = chrono::Local::now().date_naive();
        let stale = local_daily_slot(provider)
            .is_none_or(|published| published.day != today || published.roots != roots);
        if changed || stale {
            let rows = cache.local_daily(today);
            publish_local_daily(provider, LocalDaily { roots, day: today, rows });
        }
    }
    drop(cache);
    Ok(shared)
}

/// Days of local-time history kept ready for plugin probes: today plus the
/// previous 30, with a few spare for a probe that straddles midnight.
const LOCAL_DAILY_DAYS: i64 = 33;

/// Recent per-day usage in the daemon's local time zone, published after each
/// refresh. Plugin probes read it without scanning logs or waiting for the
/// cache lock, so a quota poll never repeats the background summarizer's work.
struct LocalDaily {
    roots: Vec<PathBuf>,
    day: chrono::NaiveDate,
    /// Newest first, like `ccusage daily --order desc`.
    rows: Vec<serde_json::Value>,
}

fn local_daily_slots() -> &'static Mutex<HashMap<String, Arc<LocalDaily>>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Arc<LocalDaily>>>> = OnceLock::new();
    SLOTS.get_or_init(Default::default)
}

fn local_daily_slot(provider: &str) -> Option<Arc<LocalDaily>> {
    local_daily_slots()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(provider)
        .cloned()
}

fn publish_local_daily(provider: &str, daily: LocalDaily) {
    local_daily_slots()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(provider.to_string(), Arc::new(daily));
}

impl Cache {
    fn local_daily(&mut self, today: chrono::NaiveDate) -> Vec<serde_json::Value> {
        use chrono::TimeZone;
        let first_day = today - chrono::Duration::days(LOCAL_DAILY_DAYS);
        // A UTC day of margin; exact local-day filtering happens on the rows.
        let since = chrono::Local
            .from_local_datetime(&first_day.and_hms_opt(0, 0, 0).unwrap_or_default())
            .earliest()
            .map(|at| at.with_timezone(&chrono::Utc))
            .unwrap_or_default()
            - chrono::Duration::days(1);
        let events = self.unique_events();
        let first_key = first_day.format("%Y-%m-%d").to_string();
        local_usage::aggregate_usage(
            events.into_iter().filter(|event| event.ts >= since),
            local_usage::Bucket::LocalDay,
        )
        .into_iter()
        .filter(|row| row.date >= first_key)
        .map(|row| plugin_daily_row(&row))
        .collect()
    }
}

/// The fields bundled plugins read from a ccusage daily row. Unknown cost stays
/// unknown: a partly priced day carries its known part and `costKnown: false`.
fn plugin_daily_row(row: &local_usage::UsageAggregate) -> serde_json::Value {
    let usage = &row.usage;
    let cost = (usage.cost_known || usage.cost_usd > 0.0).then_some(usage.cost_usd);
    serde_json::json!({
        "date": row.date,
        "inputTokens": usage.input_tokens,
        "outputTokens": usage.output_tokens,
        "cacheReadTokens": usage.cache_read_tokens,
        "cacheCreationTokens": usage.cache_creation_tokens,
        "reasoningOutputTokens": usage.reasoning_output_tokens,
        "totalTokens": usage.total_tokens,
        "tokensKnown": usage.tokens_known,
        "costUSD": cost,
        "costKnown": usage.cost_known,
        "models": row.models.keys().collect::<Vec<_>>(),
    })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LocalDailyQuery {
    since: Option<String>,
    home_path: Option<String>,
}

/// Plugin host entry point: `ctx.host.localUsage.daily({since, homePath})`.
/// Answers from the published summary only. `warming` means the background
/// summarizer has not finished its first pass; `not_covered` means the plugin
/// asked about a log root this daemon does not summarize.
pub(super) fn plugin_daily(plugin_id: &str, opts_json: &str) -> String {
    let status = |status: &str| serde_json::json!({ "status": status }).to_string();
    let Some(source) = local_usage::LocalSource::from_provider_id(plugin_id) else {
        return status("not_covered");
    };
    let opts: LocalDailyQuery = serde_json::from_str(opts_json).unwrap_or_default();
    // Plugins pass the same home override the native roots derive from.
    let home_var = match source {
        local_usage::LocalSource::Codex => Some("CODEX_HOME"),
        local_usage::LocalSource::Claude => Some("CLAUDE_CONFIG_DIR"),
        _ => None,
    };
    if let Some(var) = home_var {
        let normalize = |value: Option<String>| {
            value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        if normalize(opts.home_path) != normalize(std::env::var(var).ok()) {
            return status("not_covered");
        }
    }
    let Some(published) = local_daily_slot(source.id()) else {
        return status("warming");
    };
    if source.roots().ok().as_ref() != Some(&published.roots) {
        return status("not_covered");
    }
    // `since` is ccusage's inclusive YYYYMMDD in local time.
    let since = opts
        .since
        .filter(|since| since.len() == 8 && since.bytes().all(|b| b.is_ascii_digit()))
        .map(|since| format!("{}-{}-{}", &since[..4], &since[4..6], &since[6..]));
    let daily: Vec<_> = published
        .rows
        .iter()
        .filter(|row| {
            since.as_deref().is_none_or(|since| {
                row.get("date").and_then(serde_json::Value::as_str) >= Some(since)
            })
        })
        .collect();
    serde_json::json!({
        "status": "ok",
        "data": { "daily": daily, "source": "local-transcript-v2" },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn failed_scan_after_repricing_keeps_upgrade_pending_for_persistence() {
        let dir = std::env::temp_dir().join(format!(
            "usagestat-price-retry-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let known = dir.join("a.jsonl");
        let vanished = dir.join("z.jsonl");
        std::fs::write(&known, "saved").unwrap();
        let mut cache = Cache {
            version: VERSION,
            ..Default::default()
        };
        cache.files.insert(
            known.clone(),
            CachedFile {
                fingerprint: fingerprint(&known).unwrap(),
                events: vec![LocalUsageEvent {
                    model: "gpt-5.4-mini".into(),
                    input_tokens: 200_000,
                    codex_identity: Some(
                        serde_json::from_value(
                            serde_json::json!({"response_id":"r","row_key":"[]","mirrors":[]}),
                        )
                        .unwrap(),
                    ),
                    ..Default::default()
                }],
            },
        );
        assert!(
            cache
                .refresh(vec![known.clone(), vanished], |_| panic!(
                    "only saved requests exist"
                ))
                .is_err()
        );
        assert!(
            cache.pricing_as_of.is_empty(),
            "failed refresh must not finalize the upgrade"
        );
        assert!(
            cache
                .refresh(vec![known.clone()], |_| panic!(
                    "retry must preserve request counters"
                ))
                .unwrap(),
            "successful retry must request persistence"
        );
        let restored: Cache = serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(restored.pricing_as_of, super::super::pricing::version());
        assert!((restored.files[&known].events[0].cost_usd - 0.15).abs() < 1e-10);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn price_upgrade_repairs_cached_requests_and_replays_compacted_claude() {
        let dir = std::env::temp_dir().join(format!(
            "usagestat-price-upgrade-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let codex = dir.join("codex.jsonl");
        let claude = dir.join("claude.jsonl");
        let mut cache = Cache {
            version: VERSION,
            ..Default::default()
        };
        for (path, model) in [(&codex, "gpt-5.4-mini"), (&claude, "claude-haiku-5-5")] {
            std::fs::write(path, "unchanged").unwrap();
            let mut event = LocalUsageEvent {
                model: model.into(),
                input_tokens: 200_000,
                ..Default::default()
            };
            if path == &codex {
                event.codex_identity = Some(
                    serde_json::from_value(
                        serde_json::json!({"response_id":"r","row_key":"[]","mirrors":[]}),
                    )
                    .unwrap(),
                );
            }
            cache.files.insert(
                path.clone(),
                CachedFile {
                    fingerprint: fingerprint(path).unwrap(),
                    events: vec![event],
                },
            );
        }
        let mut scans = Vec::new();
        cache
            .refresh(vec![codex.clone(), claude.clone()], |path| {
                scans.push(path.to_path_buf());
                let mut event = LocalUsageEvent {
                    model: "claude-haiku-5-5".into(),
                    input_tokens: 200_000,
                    ..Default::default()
                };
                super::super::pricing::apply(&mut event);
                Ok(vec![event])
            })
            .unwrap();
        assert_eq!(scans, vec![claude.clone()]);
        assert!((cache.files[&codex].events[0].cost_usd - 0.15).abs() < 1e-10);
        assert!((cache.files[&claude].events[0].cost_usd - 0.1).abs() < 1e-10);
        cache
            .refresh(vec![codex, claude], |_| {
                panic!("upgrade must be idempotent")
            })
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_model_migration_retries_without_marking_the_cache_current() {
        let dir = std::env::temp_dir().join(format!(
            "usagestat-model-retry-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        std::fs::write(&path, [255]).unwrap();
        let identity=serde_json::from_value(serde_json::json!({"response_id":"response","row_key":serde_json::json!([0,null,0,"turn",[100,0,0,0,0],null,"unknown"]).to_string(),"mirrors":[]})).unwrap();
        let mut cache = Cache {
            version: 8,
            ..Default::default()
        };
        cache.files.insert(
            path.clone(),
            CachedFile {
                fingerprint: fingerprint(&path).unwrap(),
                events: vec![LocalUsageEvent {
                    session_id: "child".into(),
                    model: "unknown".into(),
                    input_tokens: 100,
                    codex_identity: Some(identity),
                    ..Default::default()
                }],
            },
        );
        assert!(
            cache
                .refresh(vec![path.clone()], |_| panic!(
                    "supported cached request must not be recounted"
                ))
                .is_err()
        );
        assert_eq!(cache.version, 8);
        std::fs::write(&path,"{\"type\":\"session_meta\",\"payload\":{\"id\":\"child\"}}\n{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"turn\",\"model\":\"gpt-6.1-sol\"}}").unwrap();
        assert!(
            cache
                .refresh(vec![path.clone()], |_| panic!(
                    "metadata retry must retain counters"
                ))
                .unwrap()
        );
        assert_eq!(cache.version, VERSION);
        assert_eq!(cache.files[&path].events[0].model, "gpt-6.1-sol");
        assert_eq!(cache.files[&path].events[0].input_tokens, 100);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_cache_replays_unattributed_files_without_discarding_known_requests() {
        let dir = std::env::temp_dir().join(format!(
            "usagestat-model-cache-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let known = dir.join("known.jsonl");
        let missing = dir.join("missing.jsonl");
        std::fs::write(&known, "known").unwrap();
        std::fs::write(&missing, "missing").unwrap();
        let mut cache = Cache {
            version: 8,
            // Only the model migration is under test here, not repricing.
            pricing_as_of: super::super::pricing::version(),
            ..Default::default()
        };
        for (path, model) in [(&known, "gpt-6-astra"), (&missing, "unknown")] {
            cache.files.insert(
                path.clone(),
                CachedFile {
                    fingerprint: fingerprint(path).unwrap(),
                    events: vec![LocalUsageEvent {
                        cost_known: model != "unknown",
                        model: model.into(),
                        input_tokens: 100,
                        ..Default::default()
                    }],
                },
            );
        }
        let mut replayed = Vec::new();
        cache
            .refresh(vec![known.clone(), missing.clone()], |path| {
                replayed.push(path.to_path_buf());
                Ok(vec![LocalUsageEvent {
                    model: "gpt-6.1-sol".into(),
                    input_tokens: 100,
                    ..Default::default()
                }])
            })
            .unwrap();
        assert_eq!(replayed, vec![missing.clone()]);
        assert_eq!(cache.files[&known].events[0].model, "gpt-6-astra");
        assert_eq!(cache.files[&missing].events[0].model, "gpt-6.1-sol");
        assert_eq!(
            cache
                .files
                .values()
                .flat_map(|f| &f.events)
                .map(|e| e.input_tokens)
                .sum::<u64>(),
            200
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unchanged_files_are_not_reparsed_and_appends_deletions_and_restart_invalidate() {
        let dir = std::env::temp_dir().join(format!(
            "usagestat-cache-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("fixture.jsonl");
        std::fs::write(&file, "first").unwrap();
        let mut cache = Cache::default();
        let mut scans = 0;
        let mut scan = |_: &Path| {
            scans += 1;
            Ok(vec![LocalUsageEvent {
                ts: Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
                input_tokens: 10,
                ..Default::default()
            }])
        };
        assert!(cache.refresh(vec![file.clone()], &mut scan).unwrap());
        assert!(!cache.refresh(vec![file.clone()], &mut scan).unwrap());
        let mut cache: Cache =
            serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert!(!cache.refresh(vec![file.clone()], &mut scan).unwrap());
        cache.version = VERSION - 1;
        assert!(
            cache.refresh(vec![file.clone()], &mut scan).unwrap(),
            "old parser summaries must be rescanned even when the file is unchanged"
        );
        std::fs::write(&file, "appended").unwrap();
        assert!(cache.refresh(vec![file.clone()], &mut scan).unwrap());
        assert!(cache.refresh(vec![], &mut scan).unwrap());
        assert!(cache.files.is_empty());
        assert_eq!(scans, 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn compaction_preserves_calendar_days_sessions_models_and_known_cost() {
        let mut event = LocalUsageEvent {
            ts: Utc.with_ymd_and_hms(2026, 9, 1, 23, 59, 0).unwrap(),
            session_id: "one".into(),
            model: "priced".into(),
            input_tokens: 10,
            cost_known: true,
            cost_usd: 1.0,
            cost_components: usagestat_core::usage_daily::UsageCostComponents {
                input_cost_usd: Some(1.0),
                ..usagestat_core::usage_daily::UsageCostComponents::zero()
            },
            ..Default::default()
        };
        let mut events = vec![event.clone(), event.clone()];
        event.ts += chrono::Duration::minutes(2);
        events.push(event.clone());
        event.model = "unknown".into();
        event.cost_known = false;
        event.cost_usd = 0.0;
        event.cost_components = Default::default();
        events.push(event);
        let compacted = compact(events.clone());
        assert_eq!(compacted.len(), 3);
        for report in ["daily", "weekly", "monthly", "session", "blocks"] {
            let before: serde_json::Value =
                serde_json::from_str(&local_usage::report(events.iter(), report).unwrap()).unwrap();
            let after: serde_json::Value =
                serde_json::from_str(&local_usage::report(compacted.iter(), report).unwrap())
                    .unwrap();
            assert_eq!(before, after, "{report}");
        }
    }

    #[test]
    fn codex_mirrors_reconcile_across_cached_files_and_disk_restart() {
        use serde_json::json;
        let dir = std::env::temp_dir().join(format!(
            "usagestat-ledger-cache-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let usage = json!({"input_tokens":100,"output_tokens":10,"cached_input_tokens":20,"reasoning_output_tokens":3});
        let header = json!({"type":"session_meta","payload":{"id":"thread"}});
        let context = json!({"type":"turn_context","payload":{"model":"gpt-5","turn_id":"turn"}});
        let legacy = json!({"type":"event_msg","timestamp":"2026-09-11T00:00:00Z","payload":{"type":"token_count","turn_id":"turn","info":{"last_token_usage":usage,"total_token_usage":usage}}});
        let ledger = json!({"type":"token_usage_record","timestamp":"2026-09-11T00:00:00Z","payload":{"thread_id":"thread","session_id":"thread","turn_id":"turn","response_id":"response","usage":usage,"thread_token_usage":usage}});
        let legacy_path = dir.join("legacy.jsonl");
        let ledger_path = dir.join("ledger.jsonl");
        let copy_path = dir.join("archive.jsonl");
        let lines = |row: &serde_json::Value| format!("{header}\n{context}\n{row}\n");
        std::fs::write(&legacy_path, lines(&legacy)).unwrap();
        std::fs::write(&ledger_path, lines(&ledger)).unwrap();
        std::fs::write(&copy_path, lines(&ledger)).unwrap();
        let scan = |path: &Path| {
            let mut events = Vec::new();
            local_usage::scan_codex_file(path, &mut events)?;
            Ok(events)
        };
        for initial in [&legacy_path, &ledger_path] {
            let mut cache = Cache::default();
            cache.refresh(vec![initial.clone()], scan).unwrap();
            let expected = cache.report("daily").unwrap();
            let mut cache: Cache =
                serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
            cache
                .refresh(
                    vec![legacy_path.clone(), ledger_path.clone(), copy_path.clone()],
                    scan,
                )
                .unwrap();
            assert_eq!(cache.report("daily").unwrap(), expected);
            let events: Vec<_> = cache
                .files
                .values()
                .flat_map(|file| file.events.iter())
                .collect();
            assert_eq!(super::super::codex_usage::unique(&events).len(), 1);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plugin_daily_rows_use_local_days_and_keep_unknown_cost_unknown() {
        let today = chrono::Local::now().date_naive();
        let at = |days_ago: i64, hour: u32| {
            chrono::Local
                .from_local_datetime(&(today - chrono::Duration::days(days_ago)).and_hms_opt(hour, 0, 0).unwrap())
                .earliest()
                .unwrap()
                .with_timezone(&Utc)
        };
        let event = |ts, model: &str, cost: Option<f64>| LocalUsageEvent {
            ts,
            session_id: "s".into(),
            model: model.into(),
            input_tokens: 10,
            cost_known: cost.is_some(),
            cost_usd: cost.unwrap_or(0.0),
            ..Default::default()
        };
        let mut cache = Cache::default();
        cache.files.insert(
            "a.jsonl".into(),
            CachedFile {
                fingerprint: Fingerprint { len: 0, modified: SystemTime::UNIX_EPOCH },
                events: vec![
                    // Local midnight boundaries decide the day, not UTC.
                    event(at(0, 0), "priced", Some(1.0)),
                    event(at(0, 23), "unknown", None),
                    event(at(1, 12), "unknown", None),
                    event(at(LOCAL_DAILY_DAYS + 2, 12), "priced", Some(5.0)),
                ],
            },
        );
        let rows = cache.local_daily(today);
        let dates: Vec<_> = rows.iter().map(|row| row["date"].as_str().unwrap().to_string()).collect();
        assert_eq!(
            dates,
            [today.to_string(), (today - chrono::Duration::days(1)).to_string()],
            "newest first, local days, old days dropped"
        );
        assert_eq!(rows[0]["totalTokens"], 20);
        assert_eq!(rows[0]["costUSD"], 1.0);
        assert_eq!(rows[0]["costKnown"], false);
        assert!(rows[1]["costUSD"].is_null(), "an unpriced day is not a $0 day");
    }

    #[test]
    fn plugin_daily_answers_only_for_the_summarized_log_root() {
        let status = |provider: &str, opts: &str| {
            serde_json::from_str::<serde_json::Value>(&plugin_daily(provider, opts)).unwrap()["status"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(status("cursor", "{}"), "not_covered");
        if std::env::var_os("CODEX_HOME").is_none() {
            assert_eq!(status("codex", r#"{"homePath":"/another/account"}"#), "not_covered");
        }
        // Never published in this process: the summarizer has not run yet.
        assert_eq!(status("amp", "{}"), "warming");
    }


    #[test]
    fn appends_persist_as_a_delta_and_reload_to_the_same_cache() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("cache.json");
        let (a, b) = (dir.path().join("a.jsonl"), dir.path().join("b.jsonl"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let scan = |path: &Path| {
            Ok(vec![LocalUsageEvent {
                ts: Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
                input_tokens: std::fs::metadata(path)?.len(),
                ..Default::default()
            }])
        };
        let tokens = |cache: &Cache| {
            cache
                .files
                .iter()
                .map(|(path, file)| (path.clone(), file.events[0].input_tokens))
                .collect::<Vec<_>>()
        };
        let mut cache = Cache::default();
        assert!(cache.refresh(vec![a.clone(), b.clone()], scan).unwrap());
        cache.persist(&snapshot).unwrap();
        assert!(!delta_path(&snapshot).exists(), "the first write is a snapshot");
        let daily: serde_json::Value = serde_json::from_str(&cache.report("daily").unwrap()).unwrap();
        assert_eq!(daily["daily"][0]["inputTokens"], 2);
        let written = std::fs::read(&snapshot).unwrap();

        std::fs::write(&a, "a grows").unwrap();
        assert!(cache.refresh(vec![a.clone(), b.clone()], scan).unwrap());
        cache.persist(&snapshot).unwrap();
        assert_eq!(std::fs::read(&snapshot).unwrap(), written, "an append leaves the snapshot alone");
        let delta: DeltaIn = serde_json::from_slice(&std::fs::read(delta_path(&snapshot)).unwrap()).unwrap();
        assert_eq!(delta.files.keys().collect::<Vec<_>>(), [&a], "only the changed file is rewritten");
        assert_eq!(tokens(&Cache::load(&snapshot)), tokens(&cache));
        // Reports follow the change rather than a remembered deduplication.
        let daily: serde_json::Value = serde_json::from_str(&cache.report("daily").unwrap()).unwrap();
        assert_eq!(daily["daily"][0]["inputTokens"], 8);

        // A removal after a reload still carries the earlier, unsnapshotted append.
        let mut reloaded = Cache::load(&snapshot);
        assert!(reloaded.refresh(vec![a.clone()], scan).unwrap());
        reloaded.persist(&snapshot).unwrap();
        assert_eq!(tokens(&Cache::load(&snapshot)), [(a.clone(), 7)]);

        // A delta written against another snapshot is never applied.
        let mut other = Cache::load(&snapshot);
        other.needs_snapshot = true;
        other.persist(&snapshot).unwrap();
        std::fs::write(&a, "a grows again").unwrap();
        let mut stale = Cache::load(&snapshot);
        stale.generation -= 1;
        assert!(stale.refresh(vec![a.clone()], scan).unwrap());
        stale.persist(&snapshot).unwrap();
        assert_eq!(tokens(&Cache::load(&snapshot)), [(a, 7)]);
    }

}
