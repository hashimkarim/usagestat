//! Tokscale's local Cursor cache, used only when the account-scoped dashboard
//! query fails. Ported from CodexBar 43f3c2805 (`CursorLocalJSONReader`,
//! `CursorLocalCSVReader`). The canonical `usage.json` is authoritative once it
//! exists; it is never combined with, or replaced by, migration CSVs.
use super::CsvUsageRow;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};

/// Our own guard; upstream reads the files without a byte limit.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TIMESTAMP_MS: i64 = 253_402_300_799_999;

#[derive(Debug)]
pub(super) enum Cache {
    /// Neither `usage.json` nor a migration CSV exists.
    Absent,
    /// `usage.json` exists but is unusable; CSVs must not be substituted.
    Unavailable,
    /// `cost_known` is false when any row was rejected or any in-range event
    /// omits a valid `chargedCents`, so a partial sum is never published.
    Rows {
        rows: Vec<CsvUsageRow>,
        cost_known: bool,
    },
}

pub(super) fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TOKSCALE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("cursor-cache"));
    }
    Some(dirs::home_dir()?.join(".config/tokscale/cursor-cache"))
}

/// `since`/`until` are inclusive local `YYYYMMDD` days; events after `now_ms` are excluded.
pub(super) fn load(dir: &Path, since: &str, until: &str, now_ms: i64) -> Cache {
    load_in(dir, since, until, now_ms, &Local)
}

fn load_in<Tz: TimeZone>(dir: &Path, since: &str, until: &str, now_ms: i64, tz: &Tz) -> Cache
where
    Tz::Offset: Display,
{
    let path = dir.join("usage.json");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if !meta.file_type().is_file() || meta.len() > MAX_FILE_BYTES {
                return Cache::Unavailable;
            }
            match std::fs::read(&path) {
                Ok(bytes) => load_json(&bytes, since, until, now_ms, tz),
                Err(_) => Cache::Unavailable,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            load_csvs(dir, since, until, now_ms, tz)
        }
        Err(_) => Cache::Unavailable,
    }
}

/// The dashboard rows, or with `local_cache` the Tokscale cache when the
/// dashboard failed, as `(source, rows, cost_known)`. An unusable or empty cache
/// returns the dashboard's original error.
pub(super) fn with_fallback(
    dashboard: anyhow::Result<Vec<CsvUsageRow>>,
    local_cache: bool,
    dir: Option<&Path>,
    since: &str,
    until: &str,
    now_ms: i64,
) -> anyhow::Result<(&'static str, Vec<CsvUsageRow>, bool)> {
    let error = match dashboard {
        Ok(rows) => return Ok(("dashboard", rows, true)),
        Err(error) => error,
    };
    if !local_cache {
        return Err(error);
    }
    match dir.map(|dir| load(dir, since, until, now_ms)) {
        Some(Cache::Rows { rows, cost_known }) if !rows.is_empty() && sums_fit(&rows) => {
            Ok(("tokscale", rows, cost_known))
        }
        _ => Err(error),
    }
}

/// Every downstream token sum stays representable and the cost sum finite.
fn sums_fit(rows: &[CsvUsageRow]) -> bool {
    rows.iter()
        .try_fold(0u64, |sum, row| {
            [
                row.input_no_cache,
                row.input_cache_write,
                row.cache_read,
                row.output_tokens,
                row.total_tokens,
            ]
            .into_iter()
            .try_fold(sum, |sum, value| sum.checked_add(value))
        })
        .is_some()
        && rows.iter().map(|row| row.cost_usd).sum::<f64>().is_finite()
}

fn local_day<Tz: TimeZone>(ms: i64, tz: &Tz) -> Option<String>
where
    Tz::Offset: Display,
{
    let local = DateTime::<Utc>::from_timestamp_millis(ms)?.with_timezone(tz);
    Some(local.format("%Y-%m-%d").to_string())
}

fn day_in_range(day_key: &str, since: &str, until: &str) -> bool {
    super::row_in_range(&day_key.replace('-', ""), since, until)
}

// ---- JSON ----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Amount {
    Valid(f64),
    Omitted,
    Invalid,
}

impl Amount {
    fn value(self) -> Option<f64> {
        match self {
            Amount::Valid(value) => Some(value),
            _ => None,
        }
    }
}

struct Tokens {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    total_cents: Amount,
}

impl Tokens {
    fn total(&self) -> i64 {
        [self.input, self.output, self.cache_read, self.cache_write]
            .into_iter()
            .try_fold(0i64, |sum, value| sum.checked_add(value))
            .unwrap_or(0)
    }
}

struct Row {
    id: Option<String>,
    identity: (Option<String>, String, i64),
    timestamp_ms: i64,
    model: String,
    charged_cents: Option<f64>,
    tokens: Option<Tokens>,
}

impl Row {
    fn rank_cost(&self) -> f64 {
        self.tokens
            .as_ref()
            .and_then(|tokens| tokens.total_cents.value())
            .or(self.charged_cents)
            .unwrap_or(0.0)
    }
    fn rank_tokens(&self) -> i64 {
        self.tokens.as_ref().map(Tokens::total).unwrap_or(0)
    }
}

fn load_json<Tz: TimeZone>(bytes: &[u8], since: &str, until: &str, now_ms: i64, tz: &Tz) -> Cache
where
    Tz::Offset: Display,
{
    let Ok(Value::Object(document)) = serde_json::from_slice::<Value>(bytes) else {
        return Cache::Unavailable;
    };
    if !owner_metadata_is_consistent(&document) {
        return Cache::Unavailable;
    }
    let Some(raw_rows) = document.get("usageEventsDisplay").and_then(Value::as_array) else {
        return Cache::Unavailable;
    };
    let rows: Vec<Row> = raw_rows.iter().filter_map(parse_row).collect();
    let complete = rows.len() == raw_rows.len();
    let mut out = Vec::new();
    let mut charges_known = true;
    for row in deduplicated(rows) {
        if row.timestamp_ms > now_ms {
            continue;
        }
        let Some(day_key) = local_day(row.timestamp_ms, tz) else {
            continue;
        };
        if !day_in_range(&day_key, since, until) {
            continue;
        }
        if row.charged_cents.is_none() {
            charges_known = false;
        }
        let (input, output, cache_read, cache_write) = row
            .tokens
            .as_ref()
            .map(|t| (t.input, t.output, t.cache_read, t.cache_write))
            .unwrap_or_default();
        let total = row.rank_tokens();
        let cost_usd = row.charged_cents.unwrap_or(0.0) / 100.0;
        if total == 0 && cost_usd == 0.0 {
            continue;
        }
        out.push(CsvUsageRow {
            day_key,
            model: row.model,
            input_cache_write: cache_write as u64,
            input_no_cache: input as u64,
            cache_read: cache_read as u64,
            output_tokens: output as u64,
            total_tokens: total as u64,
            cost_usd,
        });
    }
    let cost_known = complete && charges_known;
    Cache::Rows {
        rows: out,
        cost_known,
    }
}

fn owner_metadata_is_consistent(document: &Map<String, Value>) -> bool {
    let has_envelope = document.contains_key("_owner") || document.contains_key("zzTokscaleOwner");
    if !document.contains_key("_tokscale") && !has_envelope {
        return true;
    }
    let Some(legacy) = document.get("_tokscale").and_then(Value::as_object) else {
        return false;
    };
    let Some(account) = legacy.get("accountId").and_then(Value::as_str) else {
        return false;
    };
    if !is_version_one(legacy.get("version")) || account.is_empty() {
        return false;
    }
    if !has_envelope {
        return true;
    }
    let (Some(owner), Some(footer)) = (
        document.get("_owner").and_then(Value::as_object),
        document.get("zzTokscaleOwner").and_then(Value::as_object),
    ) else {
        return false;
    };
    let (Some(hash), Some(generation), Some(footer_generation)) = (
        owner.get("ownerHash").and_then(Value::as_str),
        owner.get("generation").and_then(Value::as_str),
        footer.get("generation").and_then(Value::as_str),
    ) else {
        return false;
    };
    is_version_one(owner.get("version"))
        && is_version_one(footer.get("version"))
        && is_uuid(generation)
        && generation == footer_generation
        && hash == format!("{:x}", Sha256::digest(account.as_bytes()))
}

fn is_version_one(value: Option<&Value>) -> bool {
    value.and_then(Value::as_f64) == Some(1.0)
}

fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn nonempty(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Positional fields mirror Swift's `decodeIfPresent(String)`: null is absent,
/// any other non-string rejects the row.
fn positional_string(value: Option<&Value>) -> Result<Option<String>, ()> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(nonempty(Some(&Value::String(text.clone())))),
        Some(_) => Err(()),
    }
}

fn parse_row(value: &Value) -> Option<Row> {
    let (id, conversation, timestamp, model, charged, tokens) = match value {
        Value::Object(map) => (
            nonempty(map.get("usageEventId")),
            nonempty(map.get("conversationId")),
            parse_timestamp(present(map.get("timestamp"))?)?,
            nonempty(map.get("model")),
            present(map.get("chargedCents")).map(parse_amount),
            optional(map.get("tokenUsage"), parse_tokens)?,
        ),
        Value::Array(items) => {
            if items.len() > 5 {
                return None;
            }
            (
                None,
                positional_string(items.first()).ok()?,
                parse_timestamp(present(items.get(1))?)?,
                positional_string(items.get(2)).ok()?,
                present(items.get(3)).map(parse_amount),
                optional(items.get(4), parse_tokens)?,
            )
        }
        _ => return None,
    };
    let timestamp_ms = timestamp;
    let model = model?;
    Some(Row {
        id,
        identity: (conversation, model.clone(), timestamp_ms),
        timestamp_ms,
        model,
        charged_cents: charged.and_then(Amount::value),
        tokens,
    })
}

/// `None` rejects the row; `Some(None)` is an absent or null field.
fn optional<T>(value: Option<&Value>, parse: fn(&Value) -> Option<T>) -> Option<Option<T>> {
    match present(value) {
        None => Some(None),
        Some(value) => parse(value).map(Some),
    }
}

fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

fn exact_i64(value: f64) -> Option<i64> {
    (value.is_finite()
        && value.fract() == 0.0
        && value >= -9.223_372_036_854_775_808e18
        && value < 9.223_372_036_854_775_808e18)
        .then_some(value as i64)
}

fn parse_timestamp(value: &Value) -> Option<i64> {
    let ms = match value {
        Value::String(text) => {
            let text = text.trim();
            if let Some(ms) = text
                .parse::<i64>()
                .ok()
                .or_else(|| text.parse::<f64>().ok().and_then(exact_i64))
            {
                Some(ms)
            } else {
                // Truncate submillisecond time so it never rounds into the next day.
                DateTime::parse_from_rfc3339(text)
                    .ok()
                    .map(|date| date.timestamp_millis())
            }
        }
        _ => value.as_f64().and_then(exact_i64),
    }?;
    (ms > 0 && ms <= MAX_TIMESTAMP_MS).then_some(ms)
}

fn parse_count(value: &Value) -> Option<i64> {
    let count = match value {
        Value::String(text) => text
            .parse::<i64>()
            .ok()
            .or_else(|| text.parse::<f64>().ok().and_then(exact_i64)),
        _ => value
            .as_i64()
            .or_else(|| value.as_f64().and_then(exact_i64)),
    }?;
    (count >= 0).then_some(count)
}

fn parse_amount(value: &Value) -> Amount {
    let number = match value {
        Value::String(text) => text.parse::<f64>().ok(),
        _ => value.as_f64(),
    };
    match number {
        Some(number) if number.is_finite() && number >= 0.0 => Amount::Valid(number),
        _ => Amount::Invalid,
    }
}

fn parse_tokens(value: &Value) -> Option<Tokens> {
    let count = |value: Option<&Value>| -> Option<i64> {
        match present(value) {
            None => Some(0),
            Some(value) => parse_count(value),
        }
    };
    let amount =
        |value: Option<&Value>| present(value).map(parse_amount).unwrap_or(Amount::Omitted);
    let tokens = match value {
        Value::Object(map) => Tokens {
            input: count(map.get("inputTokens"))?,
            output: count(map.get("outputTokens"))?,
            cache_read: count(map.get("cacheReadTokens"))?,
            cache_write: count(map.get("cacheWriteTokens"))?,
            total_cents: amount(map.get("totalCents")),
        },
        Value::Array(items) => {
            if items.len() > 5 {
                return None;
            }
            Tokens {
                input: count(items.first())?,
                output: count(items.get(1))?,
                cache_read: count(items.get(2))?,
                cache_write: count(items.get(3))?,
                total_cents: amount(items.get(4)),
            }
        }
        _ => return None,
    };
    [
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
    ]
    .into_iter()
    .try_fold(0i64, |sum, value| sum.checked_add(value))?;
    Some(tokens)
}

/// Repeated snapshots of one `usageEventId` count once: the largest token
/// total wins, then the largest reported cost, then the earliest row. Id-less
/// rows join the smallest event ID sharing their conversation/model/timestamp.
fn deduplicated(rows: Vec<Row>) -> Vec<Row> {
    let mut identified: HashMap<&(Option<String>, String, i64), &str> = HashMap::new();
    for row in &rows {
        let Some(id) = row.id.as_deref() else {
            continue;
        };
        let entry = identified.entry(&row.identity).or_insert(id);
        if id < *entry {
            *entry = id;
        }
    }
    let mut winners: HashMap<String, usize> = HashMap::new();
    let mut keep = vec![true; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        let Some(id) = row
            .id
            .as_deref()
            .or_else(|| identified.get(&row.identity).copied())
        else {
            continue;
        };
        if let Some(&previous) = winners.get(id) {
            let old = &rows[previous];
            let (tokens, old_tokens) = (row.rank_tokens(), old.rank_tokens());
            if tokens > old_tokens || (tokens == old_tokens && row.rank_cost() > old.rank_cost()) {
                keep[previous] = false;
                winners.insert(id.to_owned(), index);
            } else {
                keep[index] = false;
            }
        } else {
            winners.insert(id.to_owned(), index);
        }
    }
    rows.into_iter()
        .zip(keep)
        .filter_map(|(row, keep)| keep.then_some(row))
        .collect()
}

// ---- Migration CSVs ------------------------------------------------------

fn csv_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !name.starts_with('.')
                && name.starts_with("usage")
                && name.ends_with(".csv")
                && !name.starts_with("usage.backup")
        })
        .collect();
    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    paths
}

fn load_csvs<Tz: TimeZone>(dir: &Path, since: &str, until: &str, now_ms: i64, tz: &Tz) -> Cache
where
    Tz::Offset: Display,
{
    let paths = csv_paths(dir);
    if paths.is_empty() {
        return Cache::Absent;
    }
    let mut rows = Vec::new();
    for path in paths {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            rows.extend(parse_csv(&text, since, until, now_ms, tz));
        }
    }
    Cache::Rows {
        rows,
        cost_known: true,
    }
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        if ch == '"' {
            quoted = !quoted;
        } else if ch == ',' && !quoted {
            cols.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    cols.push(current);
    cols.into_iter()
        .map(|col| col.trim().replace("\"\"", "\""))
        .collect()
}

fn csv_int(text: &str) -> i64 {
    text.trim().replace(',', "").parse().unwrap_or(0)
}

fn csv_cost(text: &str) -> f64 {
    let text = text.trim();
    let lower = text.to_lowercase();
    if text.is_empty() || text == "-" || lower == "included" || lower == "nan" {
        return 0.0;
    }
    text.replace(['$', ','], "").parse().unwrap_or(0.0)
}

/// Local day key, or `None` when the date is unparseable or after `now_ms`.
fn csv_day<Tz: TimeZone>(text: &str, now_ms: i64, tz: &Tz) -> Option<String>
where
    Tz::Offset: Display,
{
    let text = text.trim();
    let utc = if let Ok(date) = DateTime::parse_from_rfc3339(text) {
        date.with_timezone(&Utc)
    } else if let Some(naive) = [
        "%Y-%m-%dT%H:%M:%S%.3f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
    {
        Utc.from_utc_datetime(&naive)
    } else {
        // Date-only rows stay on their calendar day in every time zone.
        let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
        return Some(date.format("%Y-%m-%d").to_string());
    };
    if utc.timestamp_millis() > now_ms {
        return None;
    }
    local_day(utc.timestamp_millis(), tz)
}

fn parse_csv<Tz: TimeZone>(
    text: &str,
    since: &str,
    until: &str,
    now_ms: i64,
    tz: &Tz,
) -> Vec<CsvUsageRow>
where
    Tz::Offset: Display,
{
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next().map(parse_csv_line) else {
        return Vec::new();
    };
    let has = |name: &str| header.iter().any(|col| col.to_lowercase() == name);
    // Trailing billing columns do not identify the cloud-agent schema.
    let (model_i, in_w, in_wo, read_i, out_i, cost_i) = if has("kind") && has("cloud agent id") {
        (4, 6, 7, 8, 9, 11)
    } else if has("kind") {
        (2, 4, 5, 6, 7, 9)
    } else {
        (1, 2, 3, 4, 5, 7)
    };
    let total_i = cost_i - 1;
    let has_total = header
        .get(total_i)
        .is_some_and(|col| col.to_lowercase().contains("total"));
    let mut rows = Vec::new();
    for line in lines {
        let cols = parse_csv_line(line);
        if cols.len() <= model_i.max(cost_i) {
            continue;
        }
        let model = cols[model_i].trim().to_owned();
        if model.is_empty() {
            continue;
        }
        let input = csv_int(&cols[in_wo]).max(0) as u64;
        let write = csv_int(&cols[in_w])
            .saturating_sub(csv_int(&cols[in_wo]))
            .max(0) as u64;
        let read = csv_int(&cols[read_i]).max(0) as u64;
        let output = csv_int(&cols[out_i]).max(0) as u64;
        let Some(day_key) = csv_day(&cols[0], now_ms, tz) else {
            continue;
        };
        if input == 0 && output == 0 && read == 0 && write == 0 {
            continue;
        }
        if !day_in_range(&day_key, since, until) {
            continue;
        }
        let total = if has_total {
            csv_int(&cols[total_i]).max(0) as u64
        } else {
            input
                .saturating_add(output)
                .saturating_add(read)
                .saturating_add(write)
        };
        rows.push(CsvUsageRow {
            day_key,
            model,
            input_cache_write: write,
            input_no_cache: input,
            cache_read: read,
            output_tokens: output,
            total_tokens: total,
            cost_usd: csv_cost(&cols[cost_i]),
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW_MS: i64 = 1_794_614_400_000;
    // 2026-11-13T10:00:00Z: the same local day in every zone from UTC-10 to UTC+13.
    const TS: &str = "1794564000000";

    fn event(id: Option<&str>, input: Value, cents: Value, charged: Value) -> Value {
        let mut row = json!({
            "conversationId": "synthetic-conversation", "timestamp": TS,
            "model": "synthetic-model", "chargedCents": charged,
            "tokenUsage": {
                "inputTokens": input, "outputTokens": 20,
                "cacheReadTokens": 10, "cacheWriteTokens": 5, "totalCents": cents,
            },
        });
        if let Some(id) = id {
            row["usageEventId"] = json!(id);
        }
        row
    }

    fn base() -> Value {
        event(Some("synthetic-event-a"), json!(100), json!(12.5), json!(0))
    }

    fn document(rows: Vec<Value>) -> Value {
        let account = "synthetic-account-a";
        let generation = "88AA7766-5544-4321-8877-665544332211";
        json!({
            "usageEventsDisplay": rows,
            "_owner": {
                "version": 1,
                "ownerHash": format!("{:x}", Sha256::digest(account.as_bytes())),
                "generation": generation,
            },
            "_tokscale": {"version": 1, "accountId": account},
            "zzTokscaleOwner": {"version": 1, "generation": generation},
        })
    }

    fn day() -> String {
        local_day(TS.parse().unwrap(), &Local)
            .unwrap()
            .replace('-', "")
    }

    fn load_doc(doc: &Value) -> Cache {
        let day = day();
        load_json(doc.to_string().as_bytes(), &day, &day, NOW_MS, &Local)
    }

    fn cached(cache: Cache) -> (Vec<CsvUsageRow>, bool) {
        match cache {
            Cache::Rows { rows, cost_known } => (rows, cost_known),
            other => panic!("expected rows, got {other:?}"),
        }
    }

    fn tokens(rows: &[CsvUsageRow]) -> u64 {
        rows.iter().map(|row| row.total_tokens).sum()
    }

    fn cost(rows: &[CsvUsageRow]) -> f64 {
        rows.iter().map(|row| row.cost_usd).sum()
    }

    #[test]
    fn bills_charged_cents_not_api_rate_total_cents() {
        let (rows, cost_known) = cached(load_doc(&document(vec![event(
            Some("a"),
            json!(100),
            json!(12.5),
            json!(25),
        )])));
        assert!(cost_known);
        assert_eq!(tokens(&rows), 135);
        assert_eq!(cost(&rows), 0.25);
        assert_eq!(rows[0].input_no_cache, 100);
        assert_eq!(rows[0].input_cache_write, 5);
        assert_eq!(rows[0].cache_read, 10);
        assert_eq!(rows[0].output_tokens, 20);
    }

    #[test]
    fn integer_counters_above_double_precision_remain_exact() {
        let count: u64 = 9_007_199_254_740_993;
        for input in [json!(count), json!(count.to_string())] {
            let doc = document(vec![event(Some("a"), input, json!(1), json!(0))]);
            assert_eq!(tokens(&cached(load_doc(&doc)).0), count + 35);
        }
    }

    #[test]
    fn numeric_and_iso_timestamps_describe_the_same_event() {
        for ts in [
            "1794564000000",
            " 1794564000000 ",
            "2026-11-13T10:00:00Z",
            "2026-11-13T10:00:00.000Z",
            "2026-11-13T12:00:00+02:00",
        ] {
            let mut row = base();
            row["timestamp"] = json!(ts);
            let (rows, _) = cached(load_doc(&document(vec![row])));
            assert_eq!(tokens(&rows), 135, "{ts}");
        }
    }

    #[test]
    fn submillisecond_timestamps_truncate() {
        let mut row = base();
        row["timestamp"] = json!("2026-11-13T23:59:59.9996Z");
        assert_eq!(parse_timestamp(&row["timestamp"]), Some(1_794_614_399_999));
    }

    #[test]
    fn positional_rows_support_object_and_positional_token_usage() {
        for usage in [
            json!(["100", 20.0, "10", 5, "12.5"]),
            base()["tokenUsage"].clone(),
        ] {
            let row = json!([
                "synthetic-conversation",
                1_794_564_000_000i64,
                "synthetic-model",
                "7",
                usage
            ]);
            let (rows, cost_known) = cached(load_doc(&document(vec![row])));
            assert!(cost_known);
            assert_eq!(tokens(&rows), 135);
            assert_eq!(cost(&rows), 0.07);
        }
        let extra = json!(["c", TS, "m", 0, [], "extra"]);
        let (_, cost_known) = cached(load_doc(&document(vec![base(), extra])));
        assert!(!cost_known);
    }

    #[test]
    fn empty_token_usage_keeps_metered_only_events() {
        for usage in [json!([]), json!({})] {
            let mut row = base();
            row["chargedCents"] = json!(25);
            row["tokenUsage"] = usage;
            let (rows, cost_known) = cached(load_doc(&document(vec![row])));
            assert!(cost_known);
            assert_eq!(tokens(&rows), 0);
            assert_eq!(cost(&rows), 0.25);
        }
    }

    #[test]
    fn largest_snapshot_wins_independently_of_row_order() {
        let small = event(Some("synthetic-event-a"), json!(40), json!(5), json!(0));
        for order in [vec![small.clone(), base()], vec![base(), small]] {
            let (rows, _) = cached(load_doc(&document(order)));
            assert_eq!(rows.len(), 1);
            assert_eq!(tokens(&rows), 135);
        }
    }

    #[test]
    fn equal_token_snapshots_prefer_reported_cost_then_earliest() {
        let id = Some("synthetic-event-a");
        let doc = document(vec![
            event(id, json!(100), json!(20), json!(7)),
            event(id, json!(100), json!(12.5), json!(1000)),
            event(id, json!(100), json!(20), json!(9)),
        ]);
        let (rows, _) = cached(load_doc(&doc));
        assert_eq!(tokens(&rows), 135);
        assert_eq!(cost(&rows), 0.07);
    }

    #[test]
    fn distinct_event_ids_remain_distinct() {
        let other = event(Some("synthetic-event-b"), json!(100), json!(12.5), json!(0));
        let (rows, _) = cached(load_doc(&document(vec![base(), other])));
        assert_eq!(rows.len(), 2);
        assert_eq!(tokens(&rows), 270);
    }

    #[test]
    fn idless_rows_keep_multiplicity_unless_matched_to_an_identified_event() {
        let idless = event(None, json!(100), json!(12.5), json!(0));
        let (rows_a, _) = cached(load_doc(&document(vec![idless.clone(), idless.clone()])));
        assert_eq!(tokens(&rows_a), 270);
        let (rows_b, _) = cached(load_doc(&document(vec![idless.clone(), idless, base()])));
        assert_eq!(tokens(&rows_b), 135);
    }

    #[test]
    fn ambiguous_idless_snapshots_join_only_the_smallest_identified_event() {
        let doc = document(vec![
            event(Some("b"), json!(100), json!(1), json!(0)),
            event(None, json!(200), json!(1), json!(0)),
            event(Some("a"), json!(100), json!(1), json!(0)),
        ]);
        assert_eq!(tokens(&cached(load_doc(&doc)).0), 370);
    }

    #[test]
    fn rejected_rows_keep_usable_history_but_withhold_cost() {
        for invalid in [
            "timestamp",
            "model",
            "negative",
            "overflow",
            "fractional",
            "shape",
        ] {
            let mut row = event(Some("invalid"), json!(100), json!(1), json!(0));
            match invalid {
                "timestamp" => row["timestamp"] = json!("not-a-date"),
                "model" => row["model"] = json!("  "),
                "negative" => row["tokenUsage"] = json!({"inputTokens": -1}),
                "overflow" => {
                    row["tokenUsage"] = json!({"inputTokens": i64::MAX, "outputTokens": 1})
                }
                "fractional" => row["tokenUsage"] = json!({"inputTokens": 0.5}),
                _ => row = json!(42),
            }
            let (rows, cost_known) = cached(load_doc(&document(vec![base(), row])));
            assert!(!cost_known, "{invalid}");
            assert_eq!(tokens(&rows), 135, "{invalid}");
        }
    }

    #[test]
    fn missing_or_invalid_charge_keeps_tokens_but_withholds_cost() {
        for charged in [json!(null), json!("NaN"), json!(-1)] {
            let (rows, cost_known) = cached(load_doc(&document(vec![event(
                Some("a"),
                json!(100),
                json!(1),
                charged.clone(),
            )])));
            assert!(!cost_known, "{charged}");
            assert_eq!(tokens(&rows), 135);
        }
    }

    #[test]
    fn range_filter_excludes_old_future_and_after_now_events() {
        let mut old = event(Some("old"), json!(100), json!(1), json!(100));
        old["timestamp"] = json!("2025-11-13T10:00:00Z");
        let mut future = event(Some("future"), json!(100), json!(1), json!(100));
        future["timestamp"] = json!("2027-11-13T10:00:00Z");
        let doc = document(vec![base(), old, future]);
        let (rows, _) = cached(load_json(
            doc.to_string().as_bytes(),
            "20261001",
            "20281231",
            NOW_MS,
            &Local,
        ));
        assert_eq!(tokens(&rows), 135);
        assert_eq!(cost(&rows), 0.0);
        let (rows, _) = cached(load_json(
            doc.to_string().as_bytes(),
            "20251101",
            "20251130",
            NOW_MS,
            &Local,
        ));
        assert_eq!(cost(&rows), 1.0);
    }

    #[test]
    fn inconsistent_owner_metadata_is_unavailable() {
        for invalid in [
            "hash",
            "generation",
            "version",
            "footer",
            "legacy",
            "account",
            "null",
            "uuid",
        ] {
            let mut doc = document(vec![base()]);
            match invalid {
                "hash" => doc["_owner"]["ownerHash"] = json!("0".repeat(64)),
                "generation" => {
                    doc["_owner"]["generation"] = json!("11111111-2222-3333-4444-555555555555")
                }
                "version" => doc["_owner"]["version"] = json!(2),
                "footer" => {
                    doc.as_object_mut().unwrap().remove("zzTokscaleOwner");
                }
                "legacy" => {
                    doc.as_object_mut().unwrap().remove("_tokscale");
                }
                "account" => doc["_tokscale"] = json!({"version": 1, "accountId": "different"}),
                "null" => doc["_owner"]["version"] = json!(null),
                _ => {
                    doc["_owner"]["generation"] = json!("invalid-uuid");
                    doc["zzTokscaleOwner"]["generation"] = json!("invalid-uuid");
                }
            }
            assert!(matches!(load_doc(&doc), Cache::Unavailable), "{invalid}");
        }
    }

    #[test]
    fn legacy_metadata_free_json_remains_eligible() {
        for legacy in [None, Some(json!({"version": 1, "accountId": "legacy"}))] {
            let mut doc = json!({ "usageEventsDisplay": [base()] });
            if let Some(legacy) = legacy {
                doc["_tokscale"] = legacy;
            }
            assert_eq!(tokens(&cached(load_doc(&doc)).0), 135);
        }
    }

    fn cache_dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    const CSV: &str = "Date,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost\n\
        2026-11-13T10:00:00Z,Usage,csv-model,No,150,100,10,20,180,$0.40\n";

    #[test]
    fn json_takes_priority_over_csv_even_when_unusable() {
        let day = day();
        let good = document(vec![base()]).to_string();
        let dir = cache_dir_with(&[("usage.json", &good), ("usage.csv", CSV)]);
        let (rows, _) = cached(load(dir.path(), &day, &day, NOW_MS));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "synthetic-model");

        let malformed = cache_dir_with(&[("usage.json", "{"), ("usage.csv", CSV)]);
        assert!(matches!(
            load(malformed.path(), &day, &day, NOW_MS),
            Cache::Unavailable
        ));

        let outside = cache_dir_with(&[("usage.json", &good), ("usage.csv", CSV)]);
        let (rows, _) = cached(load(outside.path(), "20250101", "20250131", NOW_MS));
        assert!(rows.is_empty());
    }

    #[test]
    fn missing_json_falls_back_to_canonical_csvs_only() {
        let day = day();
        let foreign = document(vec![event(Some("x"), json!(9000), json!(1), json!(0))]).to_string();
        let dir = cache_dir_with(&[
            ("usage.account-foreign.json", &foreign),
            ("usage.backup.json", &foreign),
            ("archive/usage.json", &foreign),
            ("usage.backup-1.csv", CSV),
            (".usage.csv", CSV),
        ]);
        assert!(matches!(
            load(dir.path(), &day, &day, NOW_MS),
            Cache::Absent
        ));

        std::fs::write(dir.path().join("usage.csv"), CSV).unwrap();
        let (rows, cost_known) = cached(load(dir.path(), &day, &day, NOW_MS));
        assert!(cost_known);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_no_cache, 100);
        assert_eq!(rows[0].input_cache_write, 50);
        assert_eq!(rows[0].total_tokens, 180);
        assert_eq!(rows[0].cost_usd, 0.40);
    }

    #[test]
    fn csv_layouts_follow_upstream_columns() {
        let legacy = "Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output,Total Tokens,Cost\n\
            2026-11-13,m,10,10,0,5,15,Included\n";
        let rows = parse_csv(legacy, "20261113", "20261113", NOW_MS, &Local);
        assert_eq!((rows[0].total_tokens, rows[0].cost_usd), (15, 0.0));
        // Header from upstream's `cursor v3 csv parses cloud agent rows`.
        let cloud = "Date,Cloud Agent ID,Automation ID,Kind,Model,Max Mode,\
            Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost\n\
            2026-11-13,agent-123,,agent,claude-opus-4-6,off,900,700,200,120,1020,0.05\n";
        let rows = parse_csv(cloud, "20261113", "20261113", NOW_MS, &Local);
        assert_eq!(rows[0].model, "claude-opus-4-6");
        assert_eq!(
            (
                rows[0].input_no_cache,
                rows[0].cache_read,
                rows[0].input_cache_write
            ),
            (700, 200, 200)
        );
        assert_eq!((rows[0].output_tokens, rows[0].cost_usd), (120, 0.05));
    }

    #[test]
    fn buckets_by_the_given_time_zone() {
        let doc = document(vec![base()]);
        let kiritimati = chrono::FixedOffset::east_opt(14 * 3600).unwrap();
        let bytes = doc.to_string();
        let (rows, _) = cached(load_json(
            bytes.as_bytes(),
            "20261114",
            "20261114",
            NOW_MS,
            &kiritimati,
        ));
        assert_eq!(rows[0].day_key, "2026-11-14");
        let (rows, _) = cached(load_json(
            bytes.as_bytes(),
            "20261114",
            "20261114",
            NOW_MS,
            &Utc,
        ));
        assert!(rows.is_empty());
        let late = "Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost\n\
            2026-11-13T10:00:00Z,m,10,10,0,5,15,1\n2026-11-13,m,10,10,0,5,15,1\n";
        let days: Vec<_> = parse_csv(late, "20261113", "20261114", NOW_MS, &kiritimati)
            .into_iter()
            .map(|row| row.day_key)
            .collect();
        assert_eq!(days, ["2026-11-14", "2026-11-13"]);
    }

    #[test]
    fn fallback_applies_only_when_enabled_and_the_dashboard_failed() {
        let day = day();
        let failed = || Err(anyhow::anyhow!("dashboard failed"));
        let good = document(vec![base()]).to_string();
        let dir = cache_dir_with(&[("usage.json", &good)]);
        let run = |dashboard, local_cache, dir: &Path| {
            with_fallback(dashboard, local_cache, Some(dir), &day, &day, NOW_MS)
        };

        let (source, rows, cost_known) = run(failed(), true, dir.path()).unwrap();
        assert_eq!((source, rows.len(), cost_known), ("tokscale", 1, true));
        let error = run(failed(), false, dir.path()).unwrap_err();
        assert_eq!(error.to_string(), "dashboard failed");
        let (source, rows, _) = run(Ok(Vec::new()), true, dir.path()).unwrap();
        assert_eq!((source, rows.len()), ("dashboard", 0));

        let partial = document(vec![base(), json!(42)]).to_string();
        let partial = cache_dir_with(&[("usage.json", &partial)]);
        assert!(!run(failed(), true, partial.path()).unwrap().2);

        let unavailable = cache_dir_with(&[("usage.json", "{"), ("usage.csv", CSV)]);
        let empty = cache_dir_with(&[
            ("usage.json", r#"{"usageEventsDisplay":[]}"#),
            ("usage.csv", CSV),
        ]);
        let absent = cache_dir_with(&[]);
        for dir in [&unavailable, &empty, &absent] {
            assert_eq!(
                run(failed(), true, dir.path()).unwrap_err().to_string(),
                "dashboard failed"
            );
        }
        assert!(with_fallback(failed(), true, None, &day, &day, NOW_MS).is_err());

        let huge = |id: &str| event(Some(id), json!(i64::MAX), json!(0), json!(0));
        let overflow = document(vec![huge("a"), huge("b"), huge("c")]).to_string();
        let overflow = cache_dir_with(&[("usage.json", &overflow)]);
        assert!(run(failed(), true, overflow.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_or_directory_canonical_json_is_unavailable() {
        let day = day();
        let good = document(vec![base()]).to_string();
        let dir = cache_dir_with(&[("usage.account-foreign.json", &good)]);
        std::os::unix::fs::symlink(
            dir.path().join("usage.account-foreign.json"),
            dir.path().join("usage.json"),
        )
        .unwrap();
        assert!(matches!(
            load(dir.path(), &day, &day, NOW_MS),
            Cache::Unavailable
        ));
        let dir = cache_dir_with(&[("usage.csv", CSV)]);
        std::fs::create_dir(dir.path().join("usage.json")).unwrap();
        assert!(matches!(
            load(dir.path(), &day, &day, NOW_MS),
            Cache::Unavailable
        ));
    }

    #[test]
    fn oversized_canonical_json_is_unavailable() {
        let day = day();
        let dir = cache_dir_with(&[("usage.csv", CSV)]);
        let file = std::fs::File::create(dir.path().join("usage.json")).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(matches!(
            load(dir.path(), &day, &day, NOW_MS),
            Cache::Unavailable
        ));
    }
}
