//! Local transcript accounting. Input token classes are mutually exclusive.
use super::{amp_usage, codex_usage, gemini_usage, pricing};
use anyhow::Result;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use usagestat_core::provider_paths;
use usagestat_core::usage_daily::{UsageCostComponents, UsageModelDaily as ModelAggregate};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct LocalUsageEvent {
    pub ts: DateTime<Utc>,
    pub session_id: String,
    pub project: String,
    /// Git branch the agent was on, when its log records one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub cost_usd: f64,
    #[serde(flatten)]
    pub cost_components: UsageCostComponents,
    pub cost_known: bool,
    pub cache_savings_usd: Option<f64>,
    // Parser evidence retained in the private cache, never exposed as usage totals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_identity: Option<codex_usage::Identity>,
}

impl LocalUsageEvent {
    pub fn metrics(&self) -> ModelAggregate {
        let total = self
            .input_tokens
            .checked_add(self.output_tokens)
            .and_then(|sum| sum.checked_add(self.cache_read_tokens))
            .and_then(|sum| sum.checked_add(self.cache_creation_tokens));
        ModelAggregate {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            reasoning_output_tokens: self.reasoning_output_tokens,
            total_tokens: total.unwrap_or(u64::MAX),
            cost_usd: self.cost_usd,
            cost_components: self.cost_components,
            cost_known: self.cost_known,
            tokens_known: total.is_some(),
            sessions: Some(1),
            cache_savings_usd: self.cache_savings_usd,
        }
    }
}

#[derive(Copy, Clone)]
pub(super) enum Bucket {
    Hour,
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UsageAggregate {
    pub date: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(flatten)]
    pub usage: ModelAggregate,
    pub models: BTreeMap<String, ModelAggregate>,
    #[serde(skip)]
    sessions: HashSet<String>,
    #[serde(skip)]
    model_sessions: BTreeMap<String, HashSet<String>>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionAggregate {
    session_id: String,
    project: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    branch: String,
    first_activity: String,
    last_activity: String,
    #[serde(flatten)]
    usage: ModelAggregate,
    models: BTreeMap<String, ModelAggregate>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct BlockAggregate {
    block_start: String,
    block_end: String,
    active: bool,
    #[serde(flatten)]
    usage: ModelAggregate,
    models: BTreeMap<String, ModelAggregate>,
    #[serde(skip)]
    sessions: HashSet<String>,
    #[serde(skip)]
    model_sessions: BTreeMap<String, HashSet<String>>,
}

pub(super) fn scan_claude_file(path: &Path, events: &mut Vec<LocalUsageEvent>) -> Result<()> {
    let file = std::fs::File::open(path)?;
    let fallback_session = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let fallback_project = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(project_from_slug)
        .unwrap_or_else(|| "unknown".to_string());
    let mut seen = HashSet::new();
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<JsonValue>(&line) else {
            continue;
        };
        let Some(usage) = v.pointer("/message/usage") else {
            continue;
        };
        let Some(ts) = parse_ts(v.get("timestamp")) else {
            continue;
        };
        let model = v
            .pointer("/message/model")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown")
            .to_string();
        let input = json_u64_value(usage, &["input_tokens", "inputTokens"]);
        let output = json_u64_value(usage, &["output_tokens", "outputTokens"]);
        let cache_read =
            json_u64_value(usage, &["cache_read_input_tokens", "cacheReadInputTokens"]);
        let cache_creation = json_u64_value(
            usage,
            &["cache_creation_input_tokens", "cacheCreationInputTokens"],
        );
        if input == 0 && output == 0 && cache_read == 0 && cache_creation == 0 {
            continue;
        }
        let session_id = v
            .get("sessionId")
            .and_then(JsonValue::as_str)
            .unwrap_or(&fallback_session)
            .to_string();
        // Claude can repeat the same assistant message's usage for several
        // content records. Only suppress an identical accounting payload with
        // a proven identity; missing IDs must not collapse unrelated requests.
        let id = |value: Option<&JsonValue>| {
            value
                .and_then(JsonValue::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        };
        let identity = match (id(v.pointer("/message/id")), id(v.get("requestId"))) {
            (Some(message), Some(request)) => Some(("message-request", message, request)),
            _ => id(v.get("uuid")).map(|uuid| ("uuid", uuid, String::new())),
        };
        if let Some(identity) = identity {
            let key = (
                session_id.clone(),
                identity,
                model.clone(),
                [input, output, cache_read, cache_creation],
            );
            if !seen.insert(key) {
                continue;
            }
        }
        let project = v
            .get("cwd")
            .and_then(JsonValue::as_str)
            .map(project_label)
            .unwrap_or_else(|| fallback_project.clone());
        let branch = v
            .get("gitBranch")
            .and_then(JsonValue::as_str)
            .filter(|b| !b.is_empty() && *b != "HEAD")
            .unwrap_or_default()
            .to_string();

        let mut event = LocalUsageEvent {
            ts,
            session_id,
            project,
            branch,
            model,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            reasoning_output_tokens: 0,
            ..LocalUsageEvent::default()
        };
        pricing::apply(&mut event);
        events.push(event);
    }
    Ok(())
}

pub(super) fn scan_codex_file(path: &Path, events: &mut Vec<LocalUsageEvent>) -> Result<()> {
    let file = std::fs::File::open(path)?;
    let fallback_session = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .trim_start_matches("rollout-")
        .to_string();
    codex_usage::scan(std::io::BufReader::new(file), fallback_session, events);
    Ok(())
}

/// Agents whose local logs record per-request token usage. Each source owns
/// its roots, file discovery and parser; reports and caching are shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalSource {
    Claude,
    Codex,
    Gemini,
    Amp,
}

impl LocalSource {
    /// Exact provider IDs used by the bundled plugins.
    pub fn from_provider_id(id: &str) -> Option<Self> {
        Some(match id {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "gemini" => Self::Gemini,
            "amp" => Self::Amp,
            _ => return None,
        })
    }

    /// Provider IDs plus the HTTP aliases accepted by local-usage routes.
    pub fn from_alias(id: &str) -> Option<Self> {
        match id {
            "anthropic" => Some(Self::Claude),
            "openai" | "chatgpt" => Some(Self::Codex),
            _ => Self::from_provider_id(id),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Amp => "amp",
        }
    }

    pub fn roots(self) -> Result<Vec<PathBuf>, provider_paths::ProviderPathError> {
        match self {
            Self::Claude => provider_paths::claude_usage_roots(),
            Self::Codex => provider_paths::codex_usage_roots(),
            Self::Gemini => provider_paths::gemini_usage_roots(),
            Self::Amp => provider_paths::amp_usage_roots(),
        }
    }

    pub fn files(self, root: &Path) -> Vec<PathBuf> {
        match self {
            Self::Claude | Self::Codex => jsonl_files(root),
            Self::Gemini => gemini_usage::files(root),
            Self::Amp => amp_usage::files(root),
        }
    }

    pub fn scan(self, path: &Path, events: &mut Vec<LocalUsageEvent>) -> Result<()> {
        match self {
            Self::Claude => scan_claude_file(path, events),
            Self::Codex => scan_codex_file(path, events),
            Self::Gemini => gemini_usage::scan_file(path, events),
            Self::Amp => amp_usage::scan_file(path, events),
        }
    }
}

pub(super) fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_jsonl_files(root, &mut out);
    out
}

fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

pub(super) fn parse_ts(value: Option<&JsonValue>) -> Option<DateTime<Utc>> {
    match value? {
        JsonValue::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc)),
        JsonValue::Number(n) => n
            .as_i64()
            .and_then(|secs| Utc.timestamp_opt(secs, 0).single()),
        _ => None,
    }
}

pub(super) fn json_u64_value(value: &JsonValue, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(JsonValue::as_u64))
        .unwrap_or(0)
}

fn project_from_slug(slug: &str) -> String {
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.replace('-', "/")
    }
}

pub(super) use usagestat_core::project::resolve as project_label;

pub(super) fn aggregate_usage<'a>(
    events: impl IntoIterator<Item = &'a LocalUsageEvent>,
    bucket: Bucket,
) -> Vec<UsageAggregate> {
    aggregate_by(events, bucket, Split::None)
}

#[derive(Copy, Clone, PartialEq)]
enum Split {
    None,
    Project,
    /// Project and git branch; events without a branch get an empty one.
    Branch,
}

/// Rows per period, optionally split by project or branch. Newest period first.
fn aggregate_by<'a>(
    events: impl IntoIterator<Item = &'a LocalUsageEvent>,
    bucket: Bucket,
    split: Split,
) -> Vec<UsageAggregate> {
    let mut map: BTreeMap<(String, String, String), UsageAggregate> = BTreeMap::new();
    for event in events {
        let key = bucket_key_for(event.ts, bucket);
        let project = if split == Split::None {
            String::new()
        } else {
            event.project.clone()
        };
        let branch = if split == Split::Branch {
            event.branch.clone()
        } else {
            String::new()
        };
        let row = map
            .entry((key.clone(), project.clone(), branch.clone()))
            .or_insert_with(|| UsageAggregate {
                date: key,
                project,
                branch: (split == Split::Branch).then_some(branch),
                ..Default::default()
            });
        let metrics = event.metrics();
        row.usage.add(&metrics);
        row.sessions.insert(event.session_id.clone());
        row.usage.sessions = Some(row.sessions.len() as u64);
        let model = row.models.entry(event.model.clone()).or_default();
        model.add(&metrics);
        let sessions = row.model_sessions.entry(event.model.clone()).or_default();
        sessions.insert(event.session_id.clone());
        model.sessions = Some(sessions.len() as u64);
    }
    map.into_values().rev().collect()
}

fn aggregate_sessions<'a>(
    events: impl IntoIterator<Item = &'a LocalUsageEvent>,
) -> Vec<SessionAggregate> {
    let mut map: BTreeMap<String, SessionAggregate> = BTreeMap::new();
    for event in events {
        let row = map
            .entry(event.session_id.clone())
            .or_insert_with(|| SessionAggregate {
                session_id: event.session_id.clone(),
                project: event.project.clone(),
                ..Default::default()
            });
        let ts = event.ts.to_rfc3339();
        if row.first_activity.is_empty() || ts < row.first_activity {
            row.first_activity = ts.clone();
        }
        if ts >= row.last_activity {
            if !event.branch.is_empty() {
                row.branch = event.branch.clone();
            }
            row.last_activity = ts;
        } else if row.branch.is_empty() {
            row.branch = event.branch.clone();
        }
        let metrics = event.metrics();
        row.usage.add(&metrics);
        row.usage.sessions = Some(1);
        let model = row.models.entry(event.model.clone()).or_default();
        model.add(&metrics);
        model.sessions = Some(1);
    }
    let mut rows: Vec<_> = map.into_values().collect();
    rows.sort_by(|a, b| b.usage.cost_usd.total_cmp(&a.usage.cost_usd));
    rows
}

fn aggregate_blocks<'a>(
    events: impl IntoIterator<Item = &'a LocalUsageEvent>,
) -> Vec<BlockAggregate> {
    let mut map: BTreeMap<i64, BlockAggregate> = BTreeMap::new();
    let now = Utc::now();
    for event in events {
        let start = event.ts.timestamp().div_euclid(5 * 3600) * (5 * 3600);
        let start_dt = Utc.timestamp_opt(start, 0).single().unwrap_or(event.ts);
        let end_dt = start_dt + ChronoDuration::hours(5);
        let row = map.entry(start).or_insert_with(|| BlockAggregate {
            block_start: start_dt.to_rfc3339(),
            block_end: end_dt.to_rfc3339(),
            active: now >= start_dt && now < end_dt,
            ..Default::default()
        });
        let metrics = event.metrics();
        row.usage.add(&metrics);
        row.sessions.insert(event.session_id.clone());
        row.usage.sessions = Some(row.sessions.len() as u64);
        let model = row.models.entry(event.model.clone()).or_default();
        model.add(&metrics);
        let sessions = row.model_sessions.entry(event.model.clone()).or_default();
        sessions.insert(event.session_id.clone());
        model.sessions = Some(sessions.len() as u64);
    }
    map.into_values().rev().collect()
}

fn bucket_key_for(ts: DateTime<Utc>, bucket: Bucket) -> String {
    match bucket {
        Bucket::Hour => ts.format("%Y-%m-%dT%H:00:00Z").to_string(),
        Bucket::Day => ts.format("%Y-%m-%d").to_string(),
        Bucket::Month => ts.format("%Y-%m").to_string(),
        Bucket::Week => {
            let date = ts.date_naive();
            let monday = date - ChronoDuration::days(date.weekday().num_days_from_monday() as i64);
            monday.format("%Y-%m-%d").to_string()
        }
    }
}

/// API-equivalent cost since `since`, optionally only in one project.
pub(super) fn spend_since(
    events: &[&LocalUsageEvent],
    since: DateTime<Utc>,
    project: Option<&str>,
) -> f64 {
    events
        .iter()
        .filter(|e| e.ts >= since && project.is_none_or(|p| e.project == p))
        .map(|e| e.cost_usd)
        .fold(0.0, |a, b| a + b)
}

/// Recost events since `since` (optionally only those on `from`) as if they ran
/// on `model`. Per source model: actual cost and the recost, at base rates.
pub(super) fn what_if(
    events: &[&LocalUsageEvent],
    model: &str,
    since: Option<DateTime<Utc>>,
    from: Option<&str>,
) -> Result<JsonValue> {
    use serde_json::json;
    let family = |m: &str| {
        m.split(['-', '.'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    if pricing::cost_as(&LocalUsageEvent::default(), model).is_none() {
        anyhow::bail!("no verified price for model {model}");
    }
    #[derive(Default)]
    struct Row {
        tokens: u64,
        actual: f64,
        actual_known: bool,
        recost: f64,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for event in events {
        if since.is_some_and(|s| event.ts < s) || from.is_some_and(|f| event.model != f) {
            continue;
        }
        let row = rows.entry(event.model.clone()).or_insert(Row {
            actual_known: true,
            ..Default::default()
        });
        row.tokens += event.metrics().total_tokens;
        row.actual += event.cost_usd;
        row.actual_known &= event.cost_known;
        row.recost += pricing::cost_as(event, model).unwrap_or(0.0);
    }
    let models: Vec<_> = rows
        .iter()
        .map(|(name, r)| {
            json!({"model": name, "totalTokens": r.tokens, "costUsd": r.actual,
                "costKnown": r.actual_known, "whatIfCostUsd": r.recost})
        })
        .collect();
    let actual: f64 = rows.values().map(|r| r.actual).sum::<f64>() + 0.0;
    let recost: f64 = rows.values().map(|r| r.recost).sum::<f64>() + 0.0;
    Ok(json!({
        "model": model, "since": since.map(|s| s.to_rfc3339()), "from": from,
        "costUsd": actual, "costKnown": rows.values().all(|r| r.actual_known),
        "whatIfCostUsd": recost,
        "crossProvider": rows.keys().any(|m| family(m) != family(model)),
        "costSource": "api-rate-estimate",
        "note": "Base API rates for the same token counts. Long-context tiers are not applied, and another model would have used a different number of tokens.",
        "models": models,
    }))
}

#[cfg(test)]
pub(super) fn report<'a>(
    events: impl Iterator<Item = &'a LocalUsageEvent>,
    report: &str,
) -> Result<String> {
    report_limited(events, report, None)
}

pub(super) fn report_limited<'a>(
    events: impl Iterator<Item = &'a LocalUsageEvent>,
    report: &str,
    limit: Option<usize>,
) -> Result<String> {
    use serde_json::json;
    let mut value = match report {
        "daily" | "models" => json!({"daily": aggregate_usage(events, Bucket::Day)}),
        "hourly" => json!({"hourly": aggregate_usage(events, Bucket::Hour)}),
        "projects" => json!({"projects": aggregate_by(events, Bucket::Day, Split::Project)}),
        "branches" => json!({"branches": aggregate_by(events, Bucket::Day, Split::Branch)}),
        "weekly" => json!({"weekly": aggregate_usage(events, Bucket::Week)}),
        "monthly" => json!({"monthly": aggregate_usage(events, Bucket::Month)}),
        "session" => json!({"sessions": aggregate_sessions(events)}),
        "session-recent" => {
            let mut rows = aggregate_sessions(events);
            rows.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
            json!({"sessions": rows})
        }
        "blocks" => json!({"blocks": aggregate_blocks(events)}),
        _ => anyhow::bail!("unsupported report"),
    };
    value["source"] = json!("local-transcript-v2");
    value["costSource"] = json!("api-rate-estimate");
    value["pricingAsOf"] = json!(pricing::AS_OF);
    value["timeZone"] = json!("UTC");
    if let Some(limit) = limit {
        let key = if report.starts_with("session") {
            "sessions"
        } else {
            "blocks"
        };
        if let Some(rows) = value.get_mut(key).and_then(JsonValue::as_array_mut) {
            let total = rows.len();
            rows.truncate(limit);
            value["totalRows"] = json!(total);
            value["limit"] = json!(limit);
        }
    }
    Ok(serde_json::to_string(&value)?)
}

#[cfg(test)]
mod claude_tests {
    use super::*;
    use serde_json::json;

    fn scan_rows(rows: Vec<JsonValue>) -> Vec<LocalUsageEvent> {
        use std::sync::atomic::{AtomicU64, Ordering};
        // The OS clock can repeat across parallel fixtures, especially on Windows.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "usagestat-claude-dedupe-{}-{}.jsonl",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let text = rows
            .iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, text).unwrap();
        let mut events = Vec::new();
        let result = scan_claude_file(&path, &mut events);
        std::fs::remove_file(path).unwrap();
        result.unwrap();
        events
    }

    fn row() -> JsonValue {
        json!({"timestamp":"2026-09-27T12:00:00Z","sessionId":"session","requestId":"request","uuid":"record",
            "message":{"id":"message","model":"claude-opus-5-5","usage":{"input_tokens":2,"output_tokens":10,
                "cache_read_input_tokens":100,"cache_creation_input_tokens":20}}})
    }

    #[test]
    fn identical_claude_usage_is_counted_once_per_message_and_request() {
        let first = row();
        let mut repeat = first.clone();
        repeat["uuid"] = json!("different-content-record");
        repeat["timestamp"] = json!("2026-09-27T12:00:01Z");
        let mut other_message = first.clone();
        other_message["message"]["id"] = json!("other-message");
        let mut other_request = first.clone();
        other_request["requestId"] = json!("other-request");
        let events = scan_rows(vec![first, repeat, other_message, other_request]);
        assert_eq!(events.len(), 3);
        let total = aggregate_usage(events.iter(), Bucket::Day);
        assert_eq!(total[0].usage.total_tokens, 396);
        assert_eq!(total[0].usage.sessions, Some(1));
        assert!((total[0].usage.cost_usd - 0.000984).abs() < 1e-12);
    }

    #[test]
    fn claude_events_keep_the_git_branch() {
        let mut on_branch = row();
        on_branch["gitBranch"] = json!("feat/x");
        let mut detached = row();
        detached["requestId"] = json!("other");
        detached["gitBranch"] = json!("HEAD");
        let events = scan_rows(vec![on_branch, detached]);
        let branches: Vec<_> = events.iter().map(|e| e.branch.as_str()).collect();
        assert_eq!(branches, ["feat/x", ""]);
    }

    #[test]
    fn unidentified_claude_records_are_preserved_unless_uuid_proves_identity() {
        let mut anonymous = row();
        anonymous.as_object_mut().unwrap().remove("requestId");
        anonymous.as_object_mut().unwrap().remove("uuid");
        anonymous["message"].as_object_mut().unwrap().remove("id");
        assert_eq!(
            scan_rows(vec![anonymous.clone(), anonymous.clone()]).len(),
            2
        );
        anonymous["uuid"] = json!("known-record");
        assert_eq!(scan_rows(vec![anonymous.clone(), anonymous]).len(), 1);
        let first = row();
        let mut changed = first.clone();
        changed["message"]["usage"]["output_tokens"] = json!(11);
        assert_eq!(
            scan_rows(vec![first, changed]).len(),
            2,
            "different usage is not a proven duplicate"
        );
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;

    fn event(ts: &str, session: &str, project: &str, tokens: u64, cost: f64) -> LocalUsageEvent {
        LocalUsageEvent {
            ts: ts.parse().unwrap(),
            session_id: session.into(),
            project: project.into(),
            model: "m".into(),
            input_tokens: tokens,
            cost_usd: cost,
            cost_known: true,
            ..Default::default()
        }
    }

    #[test]
    fn hourly_and_project_reports_split_usage_without_losing_totals() {
        let events = [
            event("2026-11-18T09:10:00Z", "a", "usagestat", 10, 1.0),
            event("2026-11-18T09:50:00Z", "b", "website", 5, 0.5),
            event("2026-11-18T13:00:00Z", "a", "usagestat", 20, 2.0),
            event("2026-11-19T01:00:00Z", "c", "usagestat", 1, 0.25),
        ];
        let hourly: JsonValue =
            serde_json::from_str(&report(events.iter(), "hourly").unwrap()).unwrap();
        let hours: Vec<_> = hourly["hourly"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["date"].as_str().unwrap(),
                    r["totalTokens"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            hours,
            [
                ("2026-11-19T01:00:00Z", 1),
                ("2026-11-18T13:00:00Z", 20),
                ("2026-11-18T09:00:00Z", 15)
            ]
        );
        assert!(hourly["hourly"][0].get("project").is_none());

        let projects: JsonValue =
            serde_json::from_str(&report(events.iter(), "projects").unwrap()).unwrap();
        let rows: Vec<_> = projects["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["date"].as_str().unwrap(),
                    r["project"].as_str().unwrap(),
                    r["costUsd"].as_f64().unwrap(),
                    r["sessions"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("2026-11-19", "usagestat", 0.25, 1),
                ("2026-11-18", "website", 0.5, 1),
                ("2026-11-18", "usagestat", 3.0, 1)
            ]
        );
    }

    #[test]
    fn branch_report_splits_a_project_by_git_branch() {
        let mut feature = event("2026-11-18T09:10:00Z", "a", "usagestat", 10, 1.0);
        feature.branch = "feat/x".into();
        let mut main = event("2026-11-18T10:00:00Z", "a", "usagestat", 5, 0.5);
        main.branch = "main".into();
        let none = event("2026-11-18T11:00:00Z", "b", "usagestat", 1, 0.25);
        let body: JsonValue =
            serde_json::from_str(&report([feature, main, none].iter(), "branches").unwrap())
                .unwrap();
        let mut rows: Vec<_> = body["branches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["branch"].as_str().unwrap(),
                    r["costUsd"].as_f64().unwrap(),
                )
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        assert_eq!(rows, [("", 0.25), ("feat/x", 1.0), ("main", 0.5)]);
    }

    #[test]
    fn what_if_recosts_the_window_on_another_model() {
        let mut opus = event("2026-11-18T09:00:00Z", "a", "app", 0, 9.0);
        opus.model = "claude-opus-5-5".into();
        opus.output_tokens = 1_000_000;
        let mut old = opus.clone();
        old.ts = "2026-10-01T00:00:00Z".parse().unwrap();
        let since = Some("2026-11-01T00:00:00Z".parse().unwrap());
        let body = what_if(&[&opus, &old], "claude-sonnet-5-5", since, None).unwrap();
        assert_eq!(body["costUsd"], 9.0);
        assert!((body["whatIfCostUsd"].as_f64().unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(body["crossProvider"], false);
        assert_eq!(body["models"][0]["model"], "claude-opus-5-5");
        assert_eq!(
            what_if(&[&opus], "gpt-6.1-sol", None, None).unwrap()["crossProvider"],
            true
        );
        assert!(what_if(&[&opus], "made-up", None, None).is_err());
    }

    #[test]
    fn session_report_spans_first_to_last_activity_with_the_latest_branch() {
        let mut early = event("2026-11-18T09:00:00Z", "a", "usagestat", 10, 1.0);
        early.branch = "feat/x".into();
        let mut late = event("2026-11-18T11:30:00Z", "a", "usagestat", 10, 1.0);
        late.branch = "main".into();
        let unbranched = event("2026-11-18T10:00:00Z", "a", "usagestat", 10, 1.0);
        let body: JsonValue =
            serde_json::from_str(&report([late, early, unbranched].iter(), "session").unwrap())
                .unwrap();
        let row = &body["sessions"][0];
        assert_eq!(row["firstActivity"], "2026-11-18T09:00:00+00:00");
        assert_eq!(row["lastActivity"], "2026-11-18T11:30:00+00:00");
        assert_eq!(row["branch"], "main");
        assert_eq!(row["costUsd"], 3.0);
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use serde_json::json;

    fn daily(source: LocalSource, root: &Path) -> Vec<JsonValue> {
        let mut events = Vec::new();
        for path in source.files(root) {
            source.scan(&path, &mut events).unwrap();
        }
        let report: JsonValue =
            serde_json::from_str(&report(events.iter(), "daily").unwrap()).unwrap();
        assert_eq!(report["source"], "local-transcript-v2");
        report["daily"].as_array().unwrap().clone()
    }

    #[test]
    fn provider_ids_and_aliases_dispatch_to_their_own_source() {
        for id in ["claude", "codex", "gemini", "amp"] {
            assert_eq!(LocalSource::from_provider_id(id).unwrap().id(), id);
        }
        assert_eq!(
            LocalSource::from_alias("anthropic"),
            Some(LocalSource::Claude)
        );
        assert_eq!(LocalSource::from_alias("chatgpt"), Some(LocalSource::Codex));
        assert_eq!(LocalSource::from_provider_id("anthropic"), None);
        assert_eq!(LocalSource::from_alias("opencode"), None);
    }

    #[test]
    fn gemini_and_amp_fixture_roots_yield_daily_model_rows() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("gemini/proj/chats");
        std::fs::create_dir_all(&chats).unwrap();
        let tokens =
            json!({"input":1000,"output":50,"cached":400,"thoughts":10,"tool":0,"total":1060});
        let rows = [
            json!({"sessionId":"g1","projectHash":"x","startTime":"2026-09-01T00:00:00Z"}),
            json!({"id":"a","timestamp":"2026-09-01T10:00:00Z","type":"gemini","model":"gemini-2.5-pro","tokens":tokens}),
            json!({"id":"a","timestamp":"2026-09-01T10:00:00Z","type":"gemini","model":"gemini-2.5-pro","tokens":tokens}),
            json!({"id":"b","timestamp":"2026-09-02T10:00:00Z","type":"gemini","model":"gemini-2.5-pro","tokens":tokens}),
        ];
        let text = rows
            .iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(chats.join("session-1.jsonl"), format!("{text}\nnot json\n")).unwrap();
        let days = daily(LocalSource::Gemini, &dir.path().join("gemini"));
        assert_eq!(days.len(), 2);
        assert_eq!(days[1]["date"], "2026-09-01");
        assert_eq!(days[1]["totalTokens"], 1060);
        assert_eq!(days[1]["cacheReadTokens"], 400);
        assert_eq!(days[1]["sessions"], 1);
        assert_eq!(days[1]["costKnown"], false);
        assert_eq!(
            days[1]["models"]["gemini-2.5-pro"]["reasoningOutputTokens"],
            10
        );

        let threads = dir.path().join("amp/threads");
        std::fs::create_dir_all(&threads).unwrap();
        std::fs::write(threads.join("T-1.json"), json!({"id":"T-1","usageLedger":{"events":[
            {"id":"e","timestamp":"2026-09-03T01:00:00Z","model":"claude-sonnet-4-5","tokens":{"input":100,"output":10}},
            {"id":"e","timestamp":"2026-09-03T01:00:00Z","model":"claude-sonnet-4-5","tokens":{"input":100,"output":10}}]}}).to_string()).unwrap();
        let days = daily(LocalSource::Amp, &threads);
        assert_eq!(days.len(), 1);
        assert_eq!(days[0]["totalTokens"], 110);
        assert_eq!(days[0]["costKnown"], true);
    }
}
