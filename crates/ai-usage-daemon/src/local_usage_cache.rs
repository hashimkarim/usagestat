//! Incremental per-profile transcript cache. Only changed files are reparsed;
//! HTTP requests share the scan and serialized reports for 30 seconds.
use super::local_usage::{self, LocalUsageEvent};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use usagestat_core::{paths, provider_paths, usage_daily};

// v7 retains Codex request identities and reconciles ledger/legacy mirrors.
const VERSION: u32 = 7;
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
    files: BTreeMap<PathBuf, CachedFile>,
    #[serde(skip)]
    checked: Option<Instant>,
    #[serde(skip)]
    reports: HashMap<String, String>,
}

impl Cache {
    fn refresh(
        &mut self,
        files: Vec<PathBuf>,
        mut scan: impl FnMut(&Path) -> Result<Vec<LocalUsageEvent>>,
    ) -> Result<bool> {
        let mut changed = self.version != VERSION;
        if changed {
            self.files.clear();
        }
        self.version = VERSION;
        let before = self.files.len();
        let files: std::collections::BTreeSet<_> = files.into_iter().collect();
        self.files.retain(|path, _| files.contains(path));
        changed |= before != self.files.len();
        // Clear serialized reports even when an I/O error interrupts a refresh.
        self.reports.clear();
        for path in files {
            let stamp = fingerprint(&path)?;
            if self
                .files
                .get(&path)
                .is_some_and(|file| file.fingerprint == stamp)
            {
                continue;
            }
            let events = compact(scan(&path)?);
            self.files.insert(
                path,
                CachedFile {
                    fingerprint: stamp,
                    events,
                },
            );
            changed = true;
        }
        self.checked = Some(Instant::now());
        Ok(changed)
    }

    fn report(&mut self, report: &str) -> Result<String> {
        self.report_limited(report, None)
    }

    fn report_limited(&mut self, report: &str, limit: Option<usize>) -> Result<String> {
        let key = format!("{report}:{limit:?}");
        if let Some(body) = self.reports.get(&key) {
            return Ok(body.clone());
        }
        let events = super::codex_usage::unique(
            &self
                .files
                .values()
                .flat_map(|file| file.events.iter())
                .collect::<Vec<_>>(),
        );
        let body = local_usage::report_limited(events.into_iter(), report, limit)?;
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
    let mut rows: BTreeMap<(String, i64, String, String), LocalUsageEvent> = BTreeMap::new();
    let mut identified = Vec::new();
    for event in events {
        // Request evidence must survive disk reload and cross-file reconciliation.
        if event.codex_identity.is_some() {
            identified.push(event);
            continue;
        }
        let key = (
            event.ts.format("%Y-%m-%d").to_string(),
            event.ts.timestamp().div_euclid(5 * 3600),
            event.session_id.clone(),
            event.model.clone(),
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
    type Key = (PathBuf, String, Vec<PathBuf>);
    type SharedCache = Arc<Mutex<Cache>>;
    static CACHES: OnceLock<Mutex<HashMap<Key, SharedCache>>> = OnceLock::new();
    let roots = match provider {
        "codex" => provider_paths::codex_usage_roots()?,
        "claude" => provider_paths::claude_usage_roots()?,
        _ => anyhow::bail!("unsupported local provider"),
    };
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
        .or_insert_with(|| {
            let cache = std::fs::read(&cache_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok())
                .filter(|cache| cache.version == VERSION)
                .unwrap_or_default();
            Arc::new(Mutex::new(cache))
        })
        .clone();
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.checked.is_none_or(|at| at.elapsed() >= TTL) {
        let files = roots
            .iter()
            .flat_map(|root| local_usage::jsonl_files(root))
            .collect();
        let changed = cache.refresh(files, |path| {
            let mut events = Vec::new();
            match provider {
                "codex" => local_usage::scan_codex_file(path, &mut events)?,
                _ => local_usage::scan_claude_file(path, &mut events)?,
            }
            Ok(events)
        })?;
        if changed {
            // Persist only token/cost metadata, never conversation contents.
            if let Err(error) = serde_json::to_vec(&*cache)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| {
                    usagestat_core::storage::write_atomic(&cache_path, &bytes).map_err(Into::into)
                })
            {
                log::warn!("could not persist local usage cache: {error}");
            }
        }
        let daily = cache.report("daily")?;
        usage_daily::ingest_json(provider, &daily)?;
    }
    cache.report_limited(report, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

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
}
