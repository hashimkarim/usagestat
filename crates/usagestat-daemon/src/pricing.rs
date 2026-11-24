//! Standard API-equivalent estimates, not subscription invoices. Rates live in
//! `pricing/models.json` (sources in docs/history-accounting.md). A daemon can
//! add newer rates from a downloaded catalog and a local override file without
//! a release; neither can remove a bundled model. Unknown model IDs never
//! inherit a family's price.
use super::local_usage::LocalUsageEvent;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use usagestat_core::usage_daily::UsageCostComponents;

const BUNDLED: &str = include_str!("../../../pricing/models.json");
/// A downloaded or override catalog larger than this is rejected.
pub(super) const MAX_CATALOG_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Tier {
    tokens: u64,
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
}

/// USD per million tokens. `above` replaces every rate for a request whose
/// input, cache-read and cache-write tokens together exceed `tokens`.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rates {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
    #[serde(default)]
    above: Option<Tier>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    ids: Vec<String>,
    /// First UTC day these rates apply (inclusive). Absent: since the model's
    /// release. A price change adds an entry instead of editing the old one,
    /// so earlier usage keeps the rate that applied then.
    #[serde(default)]
    from: Option<String>,
    #[serde(flatten)]
    rates: Rates,
}

/// Rates for one model from `from` until the next period starts.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Period {
    from: Option<chrono::NaiveDate>,
    rates: Rates,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogFile {
    schema: u32,
    as_of: String,
    models: Vec<Entry>,
}

/// One validated catalog file: lower-case model ID to rates.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Parsed {
    as_of: String,
    models: BTreeMap<String, Vec<Period>>,
}

fn valid_rate(value: f64) -> bool {
    value.is_finite() && (0.0..=10_000.0).contains(&value)
}

fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._:/-".contains(&b))
}

fn valid_date(value: &str) -> bool {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

/// Parses and validates a catalog. Any invalid entry rejects the whole file,
/// so a damaged download can never half-apply.
pub(super) fn parse(text: &str) -> anyhow::Result<Parsed> {
    if text.len() > MAX_CATALOG_BYTES {
        anyhow::bail!("pricing catalog is larger than {MAX_CATALOG_BYTES} bytes");
    }
    let file: CatalogFile = serde_json::from_str(text)?;
    if file.schema != 1 {
        anyhow::bail!("unsupported pricing catalog schema {}", file.schema);
    }
    if !valid_date(&file.as_of) {
        anyhow::bail!("pricing catalog asOf must be YYYY-MM-DD");
    }
    let mut models = BTreeMap::new();
    for entry in file.models {
        let r = entry.rates;
        let rates_ok = [r.input, r.output, r.cache_write, r.cache_read]
            .into_iter()
            .all(valid_rate)
            && r.above.is_none_or(|t| {
                t.tokens > 0
                    && [t.input, t.output, t.cache_write, t.cache_read]
                        .into_iter()
                        .all(valid_rate)
            });
        if !rates_ok || entry.ids.is_empty() {
            anyhow::bail!("pricing catalog has invalid rates for {:?}", entry.ids);
        }
        let from = match entry.from.as_deref() {
            None => None,
            Some(day) => Some(
                chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                    .map_err(|_| anyhow::anyhow!("pricing catalog from must be YYYY-MM-DD"))?,
            ),
        };
        for id in entry.ids {
            if !valid_id(&id) {
                anyhow::bail!("pricing catalog has an invalid model ID {id:?}");
            }
            let periods: &mut Vec<Period> = models.entry(id.clone()).or_default();
            if periods.iter().any(|period| period.from == from) {
                anyhow::bail!("pricing catalog lists {id} twice for the same start date");
            }
            periods.push(Period { from, rates: r });
            periods.sort_by_key(|period| period.from);
        }
    }
    Ok(Parsed {
        as_of: file.as_of,
        models,
    })
}

/// Where the active rates came from, newest layer last.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Layer {
    pub source: &'static str,
    pub as_of: String,
    pub models: usize,
}

pub(super) struct Catalog {
    /// Changes whenever any active rate changes; cached costs are repriced then.
    pub version: String,
    pub layers: Vec<Layer>,
    models: BTreeMap<String, Vec<Period>>,
}

impl Catalog {
    /// Bundled rates, then a downloaded catalog, then a local override. Later
    /// layers add models or replace a model's whole price history; they never
    /// remove a model.
    pub(super) fn merge(layers: &[(&'static str, &Parsed)]) -> Self {
        use sha2::{Digest, Sha256};
        let mut models = BTreeMap::new();
        for (_, layer) in layers {
            models.extend(layer.models.iter().map(|(id, periods)| (id.clone(), periods.clone())));
        }
        let as_of = layers
            .iter()
            .map(|(_, layer)| layer.as_of.as_str())
            .max()
            .unwrap_or_default()
            .to_string();
        let mut hash = Sha256::new();
        for (id, r) in &models {
            hash.update(format!("{id}={r:?};").as_bytes());
        }
        let digest = hash.finalize();
        Self {
            version: format!(
                "{as_of}+{}",
                digest[..4].iter().map(|b| format!("{b:02x}")).collect::<String>()
            ),
            layers: layers
                .iter()
                .map(|(source, layer)| Layer {
                    source,
                    as_of: layer.as_of.clone(),
                    models: layer.models.len(),
                })
                .collect(),
            models,
        }
    }

    /// Price history for an exact ID, optionally followed by an ISO or
    /// compact snapshot date.
    fn periods(&self, model: &str) -> Option<&[Period]> {
        let model = model.to_ascii_lowercase();
        if let Some(periods) = self.models.get(&model) {
            return Some(periods);
        }
        [8usize, 10].into_iter().find_map(|len| {
            let split = model.len().checked_sub(len + 1)?;
            let (base, suffix) = model.split_at(split);
            let suffix = suffix.strip_prefix('-')?;
            (suffix.chars().all(|c| c.is_ascii_digit() || c == '-'))
                .then(|| self.models.get(base).map(Vec::as_slice))
                .flatten()
        })
    }

    /// The rates that applied on `day` (UTC). Usage before a model's first
    /// dated price stays unpriced.
    fn rates(&self, model: &str, day: chrono::NaiveDate) -> Option<Rates> {
        self.periods(model)?
            .iter()
            .rev()
            .find(|period| period.from.is_none_or(|from| from <= day))
            .map(|period| period.rates)
    }
}

pub(super) fn bundled() -> Parsed {
    parse(BUNDLED).expect("bundled pricing/models.json is valid")
}

fn active_slot() -> &'static RwLock<Arc<Catalog>> {
    static ACTIVE: std::sync::OnceLock<RwLock<Arc<Catalog>>> = std::sync::OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(Arc::new(Catalog::merge(&[("bundled", &bundled())]))))
}

pub(super) fn active() -> Arc<Catalog> {
    active_slot()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Installs a new catalog. Returns true when its rates differ from the current ones.
pub(super) fn install(catalog: Catalog) -> bool {
    let mut slot = active_slot().write().unwrap_or_else(|e| e.into_inner());
    let changed = slot.version != catalog.version;
    *slot = Arc::new(catalog);
    changed
}

/// Identifies the active rates. Cached and saved costs record it.
pub(super) fn version() -> String {
    active().version.clone()
}

/// Newer prices published with the source, fetched at most daily.
const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/hashimkarim/usagestat/main/pricing/models.json";
const UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

fn downloaded_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(usagestat_core::paths::data_dir()?.join("pricing-catalog.json"))
}

/// A user's own rates, for a model or correction not yet published.
fn override_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(usagestat_core::paths::config_dir()?.join("pricing.json"))
}

fn read_layer(path: &std::path::Path) -> Option<Parsed> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::warn!("pricing catalog {} unreadable: {error}", path.display());
            return None;
        }
    };
    parse(&text)
        .inspect_err(|error| log::warn!("pricing catalog {} ignored: {error}", path.display()))
        .ok()
}

/// Rebuilds the active catalog from the bundled, downloaded and override
/// files. Returns true when the active rates changed.
pub(super) fn reload() -> bool {
    let downloaded = downloaded_path().ok().and_then(|path| read_layer(&path));
    let local = override_path().ok().and_then(|path| read_layer(&path));
    install(combine(&bundled(), downloaded.as_ref(), local.as_ref()))
}

fn combine(bundled: &Parsed, downloaded: Option<&Parsed>, local: Option<&Parsed>) -> Catalog {
    let mut layers = vec![("bundled", bundled)];
    // A download older than this release's own table could undo its corrections.
    layers.extend(
        downloaded
            .filter(|layer| layer.as_of >= bundled.as_of)
            .map(|layer| ("downloaded", layer)),
    );
    layers.extend(local.map(|layer| ("override", layer)));
    Catalog::merge(&layers)
}

/// Size and modification time of the files `reload` reads.
fn layer_stamps() -> Vec<Option<(u64, std::time::SystemTime)>> {
    [downloaded_path(), override_path()]
        .into_iter()
        .map(|path| {
            let meta = std::fs::metadata(path.ok()?).ok()?;
            Some((meta.len(), meta.modified().ok()?))
        })
        .collect()
}

/// Downloads the published catalog when it changed. Returns true when a new
/// valid file was saved. Sends nothing but a generic User-Agent.
fn download() -> anyhow::Result<bool> {
    use std::io::Read;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("usagestat/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let path = downloaded_path()?;
    let etag_path = path.with_extension("etag");
    let mut request = client.get(CATALOG_URL);
    if let (Ok(etag), true) = (std::fs::read_to_string(&etag_path), path.exists()) {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag.trim());
    }
    let response = request.send()?;
    let status = response.status();
    // Not published yet, or unchanged: keep the current rates quietly.
    if status == reqwest::StatusCode::NOT_MODIFIED || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !status.is_success() {
        anyhow::bail!("pricing catalog download returned {status}");
    }
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let mut body = Vec::new();
    response
        .take(MAX_CATALOG_BYTES as u64 + 1)
        .read_to_end(&mut body)?;
    let text = String::from_utf8(body)?;
    parse(&text)?;
    usagestat_core::storage::write_atomic(&path, text.as_bytes())?;
    match etag {
        Some(etag) => usagestat_core::storage::write_atomic(&etag_path, etag.as_bytes())?,
        None => {
            let _ = std::fs::remove_file(&etag_path);
        }
    }
    Ok(true)
}

/// Checks for newer prices a minute after start and then daily. New rates
/// reprice cached usage on its next refresh and repair saved estimates now.
pub(super) fn start_updates(shutdown: Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    std::thread::spawn(move || {
        let mut next = Instant::now() + Duration::from_secs(60);
        let mut stamps = layer_stamps();
        let mut tick = 0u64;
        while !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            tick += 1;
            if Instant::now() >= next {
                next = Instant::now() + UPDATE_INTERVAL;
                if let Err(error) = download() {
                    log::warn!("pricing catalog update failed: {error}");
                }
            } else if tick % 60 != 0 {
                continue;
            }
            // Also picks up an edited override file within a minute.
            let current = layer_stamps();
            if current == stamps {
                continue;
            }
            stamps = current;
            if reload() {
                log::info!("installed pricing catalog {}", version());
                if let Err(error) =
                    usagestat_core::usage_daily::backfill_api_estimates(backfill_daily)
                {
                    log::warn!("retained estimate backfill failed: {error}");
                }
            }
        }
    });
}

/// `/v1/pricing`: the active version and where its rates came from.
pub(super) fn status_json() -> String {
    let catalog = active();
    serde_json::json!({
        "version": catalog.version,
        "models": catalog.models.len(),
        "layers": catalog.layers,
        "updateUrl": CATALOG_URL,
    })
    .to_string()
}

fn rates(model: &str, day: chrono::NaiveDate) -> Option<Rates> {
    active().rates(model, day)
}

/// Whether the model has any price, on any date.
pub(super) fn is_priced(model: &str) -> bool {
    active().periods(model).is_some()
}

/// What an event's tokens would cost on another model, at base rates: cached
/// rows no longer have request boundaries, so long-context tiers are skipped.
pub(super) fn cost_as(event: &LocalUsageEvent, model: &str) -> Option<f64> {
    is_priced(model).then_some(())?;
    let mut copy = LocalUsageEvent {
        ts: event.ts,
        model: model.to_string(),
        input_tokens: event.input_tokens,
        output_tokens: event.output_tokens,
        cache_read_tokens: event.cache_read_tokens,
        cache_creation_tokens: event.cache_creation_tokens,
        ..LocalUsageEvent::default()
    };
    price(&mut copy, false);
    copy.cost_known.then_some(copy.cost_usd)
}

pub(super) fn apply(event: &mut LocalUsageEvent) {
    price(event, true);
}

fn price(event: &mut LocalUsageEvent, request_context: bool) {
    let Some(mut rates) = rates(&event.model, event.ts.date_naive()) else {
        event.cost_usd = 0.0;
        event.cost_components = UsageCostComponents::default();
        event.cost_known = false;
        event.cache_savings_usd = None;
        return;
    };
    let input = event
        .input_tokens
        .saturating_add(event.cache_read_tokens)
        .saturating_add(event.cache_creation_tokens);
    if let Some(tier) = rates.above.filter(|tier| request_context && input > tier.tokens) {
        rates.input = tier.input;
        rates.output = tier.output;
        rates.cache_write = tier.cache_write;
        rates.cache_read = tier.cache_read;
    }
    if request_context {
        let long_context = rates.above.is_some_and(|tier| input > tier.tokens);
        let Some(multiplier) =
            service_tier_multiplier(&event.model, event.service_tier.as_deref(), event.ts, long_context)
        else {
            event.cost_usd = 0.0;
            event.cost_components = UsageCostComponents::default();
            event.cost_known = false;
            event.cache_savings_usd = None;
            return;
        };
        rates.input *= multiplier;
        rates.output *= multiplier;
        rates.cache_write *= multiplier;
        rates.cache_read *= multiplier;
    }
    let input = event.input_tokens as f64 * rates.input / 1_000_000.0;
    let read = event.cache_read_tokens as f64 * rates.cache_read / 1_000_000.0;
    let write = event.cache_creation_tokens as f64 * rates.cache_write / 1_000_000.0;
    let output = event.output_tokens as f64 * rates.output / 1_000_000.0;
    event.cost_usd = input + read + write + output;
    event.cost_known = event.cost_usd.is_finite();
    event.cost_components = if event.cost_known {
        UsageCostComponents {
            input_cost_usd: Some(input),
            cache_read_cost_usd: Some(read),
            cache_write_cost_usd: Some(write),
            output_cost_usd: Some(output),
        }
    } else {
        UsageCostComponents::default()
    };
    // Read discount only. Cache-write premiums are included in cost_usd.
    event.cache_savings_usd =
        Some(event.cache_read_tokens as f64 * (rates.input - rates.cache_read) / 1_000_000.0);
}

/// Codex Fast (`priority`) and Ultrafast requests cost a multiple of every
/// Standard rate, long-context rates included. These follow OpenAI's pricing
/// table and are not in the catalog file, whose schema older daemons validate
/// strictly. Fast on a model without a published Fast rate keeps the Standard
/// estimate; Ultrafast without a published rate stays unpriced.
fn service_tier_multiplier(
    model: &str,
    tier: Option<&str>,
    ts: chrono::DateTime<chrono::Utc>,
    long_context: bool,
) -> Option<f64> {
    let model = codex_base_model(model);
    match tier {
        Some("priority") => Some(
            match model.as_str() {
                "gpt-6-astra" | "gpt-6.1-sol" | "gpt-6.1-terra" | "gpt-6-luna" | "gpt-6.2-astra"
                | "gpt-6.2-sol" => 2.0,
                // Fast rates for these were published only up to 272K input tokens.
                "gpt-5.4" | "gpt-5.4-mini" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna"
                    if !long_context =>
                {
                    2.0
                }
                "gpt-5.5" if !long_context => 2.5,
                _ => 1.0,
            },
        ),
        Some("ultrafast") => match model.as_str() {
            "gpt-6-astra" | "gpt-6.1-sol" => Some(6.0),
            // A launch promotion ends at midnight Pacific on March 1, 2027.
            "gpt-6.2-astra" | "gpt-6.2-sol" => {
                Some(if ts.timestamp() < ULTRAFAST_PROMOTION_ENDS { 3.0 } else { 6.0 })
            }
            _ => None,
        },
        _ => Some(1.0),
    }
}

/// 2027-03-01T08:00:00Z.
const ULTRAFAST_PROMOTION_ENDS: i64 = 1_803_888_000;

/// The model a Codex tier rate is published for: no `openai/` prefix or dated
/// snapshot suffix, and the unsuffixed GPT-5.6 alias is Sol.
fn codex_base_model(model: &str) -> String {
    let model = model.trim().to_ascii_lowercase();
    let model = model.strip_prefix("openai/").unwrap_or(&model);
    let base = [8usize, 10]
        .into_iter()
        .find_map(|len| {
            let split = model.len().checked_sub(len + 1)?;
            let (base, suffix) = model.split_at(split);
            let suffix = suffix.strip_prefix('-')?;
            suffix
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-')
                .then_some(base)
        })
        .unwrap_or(model);
    if base == "gpt-5.6" { "gpt-5.6-sol".into() } else { base.into() }
}

// Retained daily counters have no request boundaries. Use base rates explicitly,
// never treat a whole day's input as one long-context request.
pub(super) fn backfill_daily(row: &mut usagestat_core::usage_daily::UsageDailyRow) -> bool {
    let reported = (row.source == "ccusage").then_some((
        row.cost_usd,
        row.cost_known,
        row.cost_components,
        row.cache_savings_usd,
    ));
    let mut changed = false;
    for (name, model) in &mut row.models {
        if model.cost_known
            || !model.tokens_known
            || (row.source == "ccusage" && model.cost_usd != 0.0)
        {
            continue;
        }
        let Ok(day) = chrono::NaiveDate::parse_from_str(&row.date, "%Y-%m-%d") else {
            continue;
        };
        let mut event = LocalUsageEvent {
            ts: day.and_hms_opt(12, 0, 0).unwrap_or_default().and_utc(),
            model: name.clone(),
            input_tokens: model.input_tokens,
            output_tokens: model.output_tokens,
            cache_read_tokens: model.cache_read_tokens,
            cache_creation_tokens: model.cache_creation_tokens,
            ..Default::default()
        };
        price(&mut event, false);
        if !event.cost_known {
            continue;
        }
        row.cost_usd += event.cost_usd - model.cost_usd;
        model.cost_usd = event.cost_usd;
        model.cost_known = true;
        model.cost_components = event.cost_components;
        model.cache_savings_usd = event.cache_savings_usd;
        changed = true;
    }
    if changed {
        let tokens = row
            .models
            .values()
            .try_fold(0u64, |n, m| n.checked_add(m.total_tokens));
        row.cost_known =
            tokens == Some(row.total_tokens) && row.models.values().all(|m| m.cost_known);
        row.cost_components = UsageCostComponents::zero();
        row.cache_savings_usd = Some(0.0);
        for model in row.models.values() {
            row.cost_components.add(&model.cost_components);
            row.cache_savings_usd = row
                .cache_savings_usd
                .zip(model.cache_savings_usd)
                .map(|(a, b)| a + b);
        }
        row.pricing_as_of = Some(version());
        row.cost_source = Some("api-rate-aggregate-estimate".into());
        if let Some((cost, known, components, savings)) = reported {
            row.cost_usd = cost;
            row.cost_known = known;
            row.cost_components = components;
            row.cache_savings_usd = savings;
            row.cost_source = Some("reported-with-api-model-estimates".into());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> chrono::NaiveDate {
        chrono::Utc::now().date_naive()
    }

    fn tiered(model: &str, tier: Option<&str>, ts: &str, input: u64) -> LocalUsageEvent {
        let mut event = LocalUsageEvent {
            ts: chrono::DateTime::parse_from_rfc3339(ts).unwrap().into(),
            model: model.into(),
            input_tokens: input,
            output_tokens: 1_000_000,
            service_tier: tier.map(str::to_string),
            ..LocalUsageEvent::default()
        };
        apply(&mut event);
        event
    }

    #[test]
    fn codex_fast_and_ultrafast_multiply_every_standard_rate() {
        let at = "2026-11-24T12:00:00Z";
        let close = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} != {b}");
        // gpt-6.2-sol Standard: $2 in, $10 out per million (100K in + 1M out = $10.20).
        close(tiered("gpt-6.2-sol", None, at, 100_000).cost_usd, 10.2);
        close(tiered("gpt-6.2-sol", Some("priority"), at, 100_000).cost_usd, 20.4);
        close(tiered("gpt-6.2-sol", Some("ultrafast"), at, 100_000).cost_usd, 30.6);
        // The Ultrafast promotion ends at midnight Pacific on March 1, 2027.
        close(tiered("gpt-6.2-sol", Some("ultrafast"), "2027-03-01T07:59:59Z", 100_000).cost_usd, 30.6);
        close(tiered("gpt-6.2-sol", Some("ultrafast"), "2027-03-01T08:00:00Z", 100_000).cost_usd, 61.2);
        // gpt-6-astra Standard $10 in, $50 out: Ultrafast is 6x.
        close(tiered("gpt-6-astra", Some("ultrafast"), at, 100_000).cost_usd, 6.0 * 51.0);
        // Long-context rates are multiplied too: $4 in, $15 out above 272K.
        close(tiered("gpt-6.2-sol", Some("priority"), at, 300_000).cost_usd, 2.0 * (0.3 * 4.0 + 15.0));
        // A dated snapshot uses its base model's tier rate.
        close(tiered("gpt-6.2-sol-2026-11-01", Some("priority"), at, 100_000).cost_usd, 20.4);
    }

    #[test]
    fn unpublished_tier_rates_fall_back_or_stay_unpriced() {
        let at = "2026-11-24T12:00:00Z";
        // Fast without a published rate keeps the Standard estimate.
        let fast = tiered("gpt-6.2-astra-law", Some("priority"), at, 1_000);
        let standard = tiered("gpt-6.2-astra-law", None, at, 1_000);
        assert_eq!(fast.cost_known, standard.cost_known);
        assert_eq!(fast.cost_usd, standard.cost_usd);
        // Ultrafast without a published rate is not guessed.
        let ultra = tiered("gpt-6.1-terra", Some("ultrafast"), at, 1_000);
        assert!(is_priced("gpt-6.1-terra") && !ultra.cost_known);
        assert_eq!(ultra.cost_usd, 0.0);
    }

    /// The hard-coded table that `pricing/models.json` replaced.
    struct Legacy {
        input: f64,
        output: f64,
        write: f64,
        read: f64,
        long_context: bool,
        haiku_tier: bool,
    }

    fn legacy(model: &str) -> Option<Legacy> {
    let model = model.to_ascii_lowercase();
    // Match an exact ID, optionally followed by an ISO/compact snapshot date.
    let matches = |id: &str| {
        model == id
            || model
                .strip_prefix(id)
                .and_then(|s| s.strip_prefix('-'))
                .is_some_and(|suffix| {
                    (suffix.len() == 8 || suffix.len() == 10)
                        && suffix.chars().all(|c| c.is_ascii_digit() || c == '-')
                })
    };
    let (input, output, write, read, long_context) = if matches("gpt-6-astra") {
        (10.0, 50.0, 12.5, 1.0, true)
    } else if matches("gpt-6.1-sol") {
        (2.0, 10.0, 2.5, 0.1, true)
    } else if matches("gpt-6-sol") {
        (2.0, 10.0, 2.5, 0.2, true)
    } else if matches("gpt-6-luna") {
        (0.1, 0.5, 0.125, 0.01, true)
    } else if matches("gpt-5.6-sol") {
        (4.0, 20.0, 5.0, 0.4, true)
    } else if matches("gpt-5.6-terra") {
        (2.0, 12.0, 2.5, 0.2, true)
    } else if matches("gpt-5.6-luna") {
        (0.2, 1.2, 0.25, 0.02, true)
    } else if matches("gpt-5.5") {
        (5.0, 30.0, 5.0, 0.5, true)
    } else if matches("gpt-5.4") {
        (2.5, 15.0, 2.5, 0.25, true)
    } else if matches("gpt-5.4-mini") {
        (0.75, 4.5, 0.75, 0.075, false)
    } else if matches("gpt-5.3-codex") || matches("gpt-5.2") {
        (1.75, 14.0, 1.75, 0.175, false)
    } else if matches("gpt-5") || matches("gpt-5-codex") {
        (1.25, 10.0, 1.25, 0.125, false)
    } else if matches("claude-opus-5-5") {
        (4.0, 20.0, 5.0, 0.2, false)
    } else if matches("claude-haiku-5-5") {
        (0.1, 0.5, 0.125, 0.01, false)
    } else if matches("claude-sonnet-5-5") {
        (2.0, 10.0, 2.5, 0.1, false)
    } else if matches("claude-sonnet-5") {
        (2.0, 10.0, 2.5, 0.2, false)
    } else if [
        "claude-opus-4-5",
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-opus-5",
    ]
    .iter()
    .any(|id| matches(id))
    {
        (5.0, 25.0, 6.25, 0.5, false)
    } else if ["claude-opus-4", "claude-opus-4-1"]
        .iter()
        .any(|id| matches(id))
    {
        (15.0, 75.0, 18.75, 1.5, false)
    } else if ["claude-sonnet-4", "claude-sonnet-4-5", "claude-sonnet-4-6"]
        .iter()
        .any(|id| matches(id))
    {
        (3.0, 15.0, 3.75, 0.3, false)
    } else if matches("claude-haiku-4-5") {
        (1.0, 5.0, 1.25, 0.1, false)
    } else if matches("claude-3-5-haiku") {
        (0.8, 4.0, 1.0, 0.08, false)
    } else {
        return None;
    };
    Some(Legacy {
        input,
        output,
        write,
        read,
        long_context,
        haiku_tier: matches("claude-haiku-5-5"),
    })
}

    #[test]
    fn bundled_catalog_reproduces_the_previous_rate_table_exactly() {
        let catalog = Catalog::merge(&[("bundled", &bundled())]);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        // Models added after the old table are covered by the catalog tests.
        let previous: Vec<String> =
            catalog.models.keys().filter(|id| legacy(id).is_some()).cloned().collect();
        assert_eq!(previous.len(), 30, "every model of the old table is still bundled");
        let mut ids = previous.clone();
        for id in &previous {
            ids.extend([
                format!("{id}-20260101"),
                format!("{id}-2026-01-01"),
                format!("{id}-x"),
                format!("{id}-foo"),
                format!("{id}-2026010"),
                id.to_ascii_uppercase(),
            ]);
        }
        ids.extend(["gpt-5.4-mini-x", "claude-sonnet-5-5-foo", "gpt-6", "claude"].map(String::from));
        for id in ids {
            let new = catalog.rates(&id, today());
            let old = legacy(&id);
            assert_eq!(new.is_some(), old.is_some(), "{id}");
            let (Some(new), Some(old)) = (new, old) else { continue };
            assert!(
                close(new.input, old.input) && close(new.output, old.output)
                    && close(new.cache_write, old.write) && close(new.cache_read, old.read),
                "{id}"
            );
            let tier = if old.haiku_tier {
                Some((100_000, 5.0, 5.0))
            } else if old.long_context {
                Some((272_000, 2.0, 1.5))
            } else {
                None
            };
            match (new.above, tier) {
                (None, None) => {}
                (Some(t), Some((tokens, input, output))) => assert!(
                    t.tokens == tokens
                        && close(t.input, old.input * input)
                        && close(t.cache_write, old.write * input)
                        && close(t.cache_read, old.read * input)
                        && close(t.output, old.output * output),
                    "{id} tier"
                ),
                _ => panic!("{id}: tier mismatch"),
            }
        }
    }

    #[test]
    fn catalogs_are_validated_whole_and_layers_only_add_or_replace() {
        let file = |models: &str| format!(r#"{{"schema":1,"asOf":"2026-12-01","models":[{models}]}}"#);
        let astra = r#"{"ids":["gpt-9-test"],"input":8,"output":40,"cacheWrite":10,"cacheRead":0.4,
            "above":{"tokens":272000,"input":16,"output":60,"cacheWrite":20,"cacheRead":0.8}}"#;
        let downloaded = parse(&file(astra)).unwrap();
        for bad in [
            file(r#"{"ids":["Upper"],"input":1,"output":1,"cacheWrite":1,"cacheRead":1}"#),
            file(r#"{"ids":["x"],"input":-1,"output":1,"cacheWrite":1,"cacheRead":1}"#),
            file(r#"{"ids":["x"],"input":1e9,"output":1,"cacheWrite":1,"cacheRead":1}"#),
            file(r#"{"ids":[],"input":1,"output":1,"cacheWrite":1,"cacheRead":1}"#),
            file(&format!("{astra},{astra}")),
            r#"{"schema":2,"asOf":"2026-12-01","models":[]}"#.to_string(),
            r#"{"schema":1,"asOf":"soon","models":[]}"#.to_string(),
            "x".repeat(MAX_CATALOG_BYTES + 1),
        ] {
            assert!(parse(&bad).is_err(), "{bad:.80}");
        }
        let base = bundled();
        let before = Catalog::merge(&[("bundled", &base)]);
        let after = Catalog::merge(&[("bundled", &base), ("downloaded", &downloaded)]);
        assert!(before.rates("gpt-9-test", today()).is_none());
        assert_eq!(after.rates("gpt-9-test-2026-11-20", today()).unwrap().input, 8.0);
        // Nothing bundled disappears, and the version follows the rates.
        assert!(base.models.keys().all(|id| after.rates(id, today()).is_some()));
        assert_ne!(before.version, after.version);
        assert!(after.version.starts_with("2026-12-01+"));
        assert_eq!(before.version, Catalog::merge(&[("bundled", &base)]).version);

        // A price change is a new dated period: earlier usage keeps its rate.
        let rise = parse(&file(&format!(
            "{astra},{}",
            astra.replace(r#""input":8,"#, r#""from":"2027-01-01","input":12,"#)
        )))
        .unwrap();
        let dated = Catalog::merge(&[("bundled", &base), ("downloaded", &rise)]);
        let day = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(dated.rates("gpt-9-test", day("2026-12-31")).unwrap().input, 8.0);
        assert_eq!(dated.rates("gpt-9-test", day("2027-01-01")).unwrap().input, 12.0);
        let first_dated = parse(&file(&astra.replace(r#""input":8,"#, r#""from":"2027-01-01","input":8,"#))).unwrap();
        assert!(Catalog::merge(&[("d", &first_dated)]).rates("gpt-9-test", day("2026-06-01")).is_none());
        assert!(parse(&file(&format!("{astra},{}", astra.replace("8,", "9,")))).is_err(), "same start twice");

        // An outdated download is ignored; a local override wins over both.
        let stale = parse(&file(astra).replace("2026-12-01", "2026-01-01")).unwrap();
        assert!(combine(&base, Some(&stale), None).rates("gpt-9-test", today()).is_none());
        let local = parse(&file(&astra.replace(r#""input":8,"#, r#""input":9,"#))).unwrap();
        let layered = combine(&base, Some(&downloaded), Some(&local));
        assert_eq!(layered.rates("gpt-9-test", today()).unwrap().input, 9.0);
        assert_eq!(
            layered.layers.iter().map(|layer| layer.source).collect::<Vec<_>>(),
            ["bundled", "downloaded", "override"]
        );
    }

    #[test]
    fn cost_as_reprices_tokens_at_another_models_base_rates() {
        let event = LocalUsageEvent {
            model: "claude-opus-5-5".into(),
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 1_000_000,
            ..LocalUsageEvent::default()
        };
        // Sonnet 5.5: $2 input, $10 output, $0.10 cached input per million.
        assert!((cost_as(&event, "claude-sonnet-5-5").unwrap() - 12.1).abs() < 1e-9);
        assert_eq!(cost_as(&event, "not-a-model"), None);
    }

    #[test]
    fn haiku_55_prompt_tier_counts_all_input_classes_but_not_output() {
        let mut event = LocalUsageEvent {
            model: "claude-haiku-5-5".into(),
            input_tokens: 10_000,
            cache_read_tokens: 80_000,
            cache_creation_tokens: 10_000,
            output_tokens: 200_000,
            ..Default::default()
        };
        apply(&mut event);
        assert!(event.cost_known);
        assert!((event.cost_usd - 0.10305).abs() < 1e-10);
        event.input_tokens += 1;
        apply(&mut event);
        assert!((event.cost_usd - 0.5152505).abs() < 1e-10);
        assert!((event.cache_savings_usd.unwrap() - 0.036).abs() < 1e-10);
    }

    #[test]
    fn historical_codex_ids_have_verified_rates_but_spark_has_no_api_price() {
        for (model, expected) in [
            ("gpt-5-codex", 11.375),
            ("gpt-5.2", 15.925),
            ("gpt-5.4-mini", 5.325),
        ] {
            let mut event = LocalUsageEvent {
                model: model.into(),
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: 1_000_000,
                ..Default::default()
            };
            apply(&mut event);
            assert!(event.cost_known, "{model}");
            assert!((event.cost_usd - expected).abs() < 1e-10, "{model}");
        }
        let mut event = LocalUsageEvent {
            model: "gpt-5.3-codex-spark".into(),
            input_tokens: 1000,
            ..Default::default()
        };
        apply(&mut event);
        assert!(!event.cost_known);
        assert_eq!(event.cache_savings_usd, None);
    }

    #[test]
    fn ccusage_model_backfill_preserves_reported_daily_cost() {
        let mut row: usagestat_core::usage_daily::UsageDailyRow=serde_json::from_value(serde_json::json!({
            "providerId":"codex","displayName":"Codex","date":"2026-11-07","source":"ccusage",
            "inputTokens":1177,"outputTokens":54,"cacheReadTokens":201344,"cacheCreationTokens":0,
            "reasoningOutputTokens":0,"totalTokens":202575,"costUsd":42,"costKnown":true,"ingestedAt":"old",
            "models":{"gpt-6.1-sol":{"inputTokens":1177,"outputTokens":54,"cacheReadTokens":201344,
            "cacheCreationTokens":0,"reasoningOutputTokens":0,"totalTokens":202575,"costUsd":0,"costKnown":false,"tokensKnown":true}}
        })).unwrap();
        row.models.insert(
            "gpt-6-sol".into(),
            usagestat_core::usage_daily::UsageModelDaily {
                cost_usd: -1.0,
                cost_known: false,
                input_tokens: 1000,
                total_tokens: 1000,
                ..Default::default()
            },
        );
        assert!(backfill_daily(&mut row));
        assert_eq!(row.models["gpt-6-sol"].cost_usd, -1.0);
        assert!(!row.models["gpt-6-sol"].cost_known);
        assert_eq!(row.cost_usd, 42.0);
        assert!(row.cost_known);
        let model = &row.models["gpt-6.1-sol"];
        assert!(model.cost_known);
        assert!((model.cost_usd - 0.0230284).abs() < 1e-9);
        assert_eq!(
            row.cost_source.as_deref(),
            Some("reported-with-api-model-estimates")
        );
        assert!(!backfill_daily(&mut row));
    }

    #[test]
    fn retained_daily_usage_gets_base_rate_backfill_without_request_context() {
        let mut row: usagestat_core::usage_daily::UsageDailyRow = serde_json::from_value(serde_json::json!({
            "providerId":"codex","displayName":"Codex","date":"2026-10-01","source":"local-transcript-v2",
            "inputTokens":1000000,"outputTokens":0,"cacheReadTokens":1000000,"cacheCreationTokens":0,
            "reasoningOutputTokens":0,"totalTokens":2000000,"costUsd":0,"costKnown":false,"ingestedAt":"old",
            "models":{"gpt-6.1-sol":{"inputTokens":1000000,"outputTokens":0,"cacheReadTokens":1000000,
            "cacheCreationTokens":0,"reasoningOutputTokens":0,"totalTokens":2000000,"costUsd":0,"costKnown":false,"tokensKnown":true}}
        })).unwrap();
        assert!(backfill_daily(&mut row));
        assert!(row.cost_known);
        assert!((row.cost_usd - 2.1).abs() < 1e-9);
        assert_eq!(
            row.cost_source.as_deref(),
            Some("api-rate-aggregate-estimate")
        );
        assert!(!backfill_daily(&mut row));
    }

    #[test]
    fn sol_61_prices_requests_and_preserves_previous_sol_cache_rates() {
        for model in ["gpt-6.1-sol", "gpt-6.1-sol-20260929"] {
            let mut event = LocalUsageEvent {
                model: model.into(),
                input_tokens: 170_000,
                cache_read_tokens: 100_000,
                cache_creation_tokens: 2_000,
                output_tokens: 1_000,
                reasoning_output_tokens: 500,
                ..Default::default()
            };
            apply(&mut event);
            assert!(event.cost_known);
            assert!((event.cost_usd - 0.365).abs() < 1e-9);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.01));
            assert_eq!(event.cost_components.cache_write_cost_usd, Some(0.005));
            assert_eq!(event.cost_components.output_cost_usd, Some(0.01));
            event.input_tokens += 1;
            apply(&mut event);
            assert!((event.cost_usd - 0.725004).abs() < 1e-9);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.02));
            assert_eq!(event.cost_components.output_cost_usd, Some(0.015));
            event.model = "gpt-6-sol".into();
            apply(&mut event);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.04));
            event.model = "gpt-6.1-sol-future".into();
            apply(&mut event);
            assert!(!event.cost_known);
        }
    }

    #[test]
    fn sonnet_55_has_explicit_rates_without_pricing_unknown_variants() {
        for model in ["claude-sonnet-5-5", "claude-sonnet-5-5-20260928"] {
            let mut event = LocalUsageEvent {
                model: model.into(),
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: 1_000_000,
                cache_creation_tokens: 1_000_000,
                ..Default::default()
            };
            apply(&mut event);
            assert!(event.cost_known);
            assert!((event.cost_usd - 14.6).abs() < 1e-9);
            assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.1));
            assert_eq!(event.cost_components.cache_write_cost_usd, Some(2.5));
            event.model = "claude-sonnet-5-5-future".into();
            apply(&mut event);
            assert!(!event.cost_known);
        }
    }

    #[test]
    fn astra_is_priced_unknowns_are_not_guessed_and_long_context_is_per_request() {
        let mut event = LocalUsageEvent {
            model: "gpt-6-astra".into(),
            input_tokens: 100_000,
            cache_read_tokens: 100_000,
            output_tokens: 1_000,
            ..Default::default()
        };
        apply(&mut event);
        assert!((event.cost_usd - 1.15).abs() < 1e-9);
        assert_eq!(event.cache_savings_usd, Some(0.9));
        assert_eq!(event.cost_components.input_cost_usd, Some(1.0));
        assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.1));
        assert_eq!(event.cost_components.cache_write_cost_usd, Some(0.0));
        assert_eq!(event.cost_components.output_cost_usd, Some(0.05));
        event.input_tokens = 200_000;
        apply(&mut event);
        assert!((event.cost_usd - 4.275).abs() < 1e-9);
        assert_eq!(event.cost_components.input_cost_usd, Some(4.0));
        assert_eq!(event.cost_components.cache_read_cost_usd, Some(0.2));
        assert_eq!(event.cost_components.output_cost_usd, Some(0.075));
        for model in ["unknown", "gpt-5-future", "some-codex-model"] {
            event.model = model.into();
            apply(&mut event);
            assert!(!event.cost_known);
            assert_eq!(event.cache_savings_usd, None);
            assert_eq!(event.cost_components, UsageCostComponents::default());
        }
    }

    #[test]
    fn cache_writes_and_reasoning_are_priced_once_before_aggregation() {
        let mut event = LocalUsageEvent {
            model: "gpt-6-astra".into(),
            input_tokens: 100_000,
            cache_read_tokens: 100_000,
            cache_creation_tokens: 2_000,
            output_tokens: 1_000,
            reasoning_output_tokens: 500,
            ..Default::default()
        };
        apply(&mut event);
        assert_eq!(event.cost_components.cache_write_cost_usd, Some(0.025));
        assert_eq!(event.cost_components.output_cost_usd, Some(0.05));
        assert!((event.cost_usd - 1.175).abs() < 1e-9);
        let mut total = event.metrics();
        event.input_tokens = 200_000;
        apply(&mut event);
        assert_eq!(event.cost_components.cache_write_cost_usd, Some(0.05));
        assert!((event.cost_usd - 4.325).abs() < 1e-9);
        total.add(&event.metrics());
        assert!((total.cost_usd - 5.5).abs() < 1e-9);
        let c = total.cost_components;
        let sum = c.input_cost_usd.unwrap()
            + c.cache_read_cost_usd.unwrap()
            + c.cache_write_cost_usd.unwrap()
            + c.output_cost_usd.unwrap();
        assert!((sum - total.cost_usd).abs() < 1e-9);
    }
}
