//! Local transcript accounting. Input token classes are mutually exclusive.
use super::{codex_usage, pricing};
use anyhow::Result;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use usagestat_core::usage_daily::{UsageCostComponents, UsageModelDaily as ModelAggregate};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct LocalUsageEvent {
    pub ts: DateTime<Utc>,
    pub session_id: String,
    pub project: String,
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
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UsageAggregate {
    pub date: String,
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
            value.and_then(JsonValue::as_str).filter(|id| !id.is_empty())
                .map(str::to_owned)
        };
        let identity = match (id(v.pointer("/message/id")), id(v.get("requestId"))) {
            (Some(message), Some(request)) => Some(("message-request", message, request)),
            _ => id(v.get("uuid")).map(|uuid| ("uuid", uuid, String::new())),
        };
        if let Some(identity) = identity {
            let key = (session_id.clone(), identity, model.clone(), [input, output, cache_read, cache_creation]);
            if !seen.insert(key) {
                continue;
            }
        }
        let project = v
            .get("cwd")
            .and_then(JsonValue::as_str)
            .map(project_label)
            .unwrap_or_else(|| fallback_project.clone());

        let mut event = LocalUsageEvent {
            ts,
            session_id,
            project,
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

pub(super) fn project_label(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

pub(super) fn aggregate_usage<'a>(
    events: impl IntoIterator<Item = &'a LocalUsageEvent>,
    bucket: Bucket,
) -> Vec<UsageAggregate> {
    let mut map: BTreeMap<String, UsageAggregate> = BTreeMap::new();
    for event in events {
        let key = bucket_key_for(event.ts, bucket);
        let row = map.entry(key.clone()).or_insert_with(|| UsageAggregate {
            date: key,
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
        row.last_activity = row.last_activity.clone().max(event.ts.to_rfc3339());
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
        Bucket::Day => ts.format("%Y-%m-%d").to_string(),
        Bucket::Month => ts.format("%Y-%m").to_string(),
        Bucket::Week => {
            let date = ts.date_naive();
            let monday = date - ChronoDuration::days(date.weekday().num_days_from_monday() as i64);
            monday.format("%Y-%m-%d").to_string()
        }
    }
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
        "weekly" => json!({"weekly": aggregate_usage(events, Bucket::Week)}),
        "monthly" => json!({"monthly": aggregate_usage(events, Bucket::Month)}),
        "session" => json!({"sessions": aggregate_sessions(events)}),
        "blocks" => json!({"blocks": aggregate_blocks(events)}),
        _ => anyhow::bail!("unsupported report"),
    };
    value["source"] = json!("local-transcript-v2");
    value["costSource"] = json!("api-rate-estimate");
    value["pricingAsOf"] = json!(pricing::AS_OF);
    value["timeZone"] = json!("UTC");
    if let Some(limit) = limit {
        let key = if report == "session" {
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
        let path = std::env::temp_dir().join(format!("usagestat-claude-dedupe-{}-{}.jsonl", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap()));
        let text = rows.iter().map(JsonValue::to_string).collect::<Vec<_>>().join("\n");
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
    fn unidentified_claude_records_are_preserved_unless_uuid_proves_identity() {
        let mut anonymous = row();
        anonymous.as_object_mut().unwrap().remove("requestId");
        anonymous.as_object_mut().unwrap().remove("uuid");
        anonymous["message"].as_object_mut().unwrap().remove("id");
        assert_eq!(scan_rows(vec![anonymous.clone(), anonymous.clone()]).len(), 2);
        anonymous["uuid"] = json!("known-record");
        assert_eq!(scan_rows(vec![anonymous.clone(), anonymous]).len(), 1);
        let first = row();
        let mut changed = first.clone();
        changed["message"]["usage"]["output_tokens"] = json!(11);
        assert_eq!(scan_rows(vec![first, changed]).len(), 2, "different usage is not a proven duplicate");
    }
}
