//! Bounded chart views over non-additive polling snapshots.
use super::{SnapshotRecord, response_json};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::BufRead;

#[derive(Default)]
struct Query {
    since: Option<String>,
    until: Option<String>,
    group: String,
    chart: bool,
}

impl Query {
    fn parse(query: &str) -> Result<Self, &'static str> {
        let mut out = Self::default();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair
                .split_once('=')
                .ok_or("Expected key=value query parameters")?;
            match key {
                "since" | "until" => {
                    if value.len() != 10 || NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() {
                        return Err("Dates must be YYYY-MM-DD");
                    }
                    let slot = if key == "since" {
                        &mut out.since
                    } else {
                        &mut out.until
                    };
                    if slot.is_some() {
                        return Err("Duplicate date parameter");
                    }
                    *slot = Some(value.to_string());
                }
                "group"
                    if matches!(value, "raw" | "hour" | "day" | "week" | "month")
                        && out.group.is_empty() =>
                {
                    out.group = value.into()
                }
                "view" if value == "chart" && !out.chart => out.chart = true,
                _ => {
                    return Err(
                        "Supported parameters: since, until, group=raw|hour|day|week|month, view=chart",
                    );
                }
            }
        }
        if out
            .since
            .as_ref()
            .zip(out.until.as_ref())
            .is_some_and(|(a, b)| a > b)
        {
            return Err("since must not follow until");
        }
        Ok(out)
    }
}

pub(super) fn serve(provider: Option<&str>, query: &str) -> String {
    let parameters = query;
    let query = match Query::parse(parameters) {
        Ok(query) => query,
        Err(error) => {
            return response_json(
                400,
                "Bad Request",
                &serde_json::json!({"error":error}).to_string(),
            );
        }
    };
    type Key = (std::path::PathBuf, Option<String>, String);
    struct Cached {
        len: u64,
        modified: std::time::SystemTime,
        response: String,
    }
    static CACHE: std::sync::OnceLock<std::sync::Mutex<BTreeMap<Key, Cached>>> =
        std::sync::OnceLock::new();
    let Some(path) = usagestat_core::paths::data_dir()
        .ok()
        .map(|dir| dir.join("history.jsonl"))
    else {
        return response_json(200, "OK", "[]");
    };
    let stamp = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| Some((meta.len(), meta.modified().ok()?)));
    let key = (
        path.clone(),
        provider.map(str::to_ascii_lowercase),
        parameters.to_string(),
    );
    let cache = CACHE.get_or_init(Default::default);
    if query.chart {
        if let Some(cached) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            if stamp == Some((cached.len, cached.modified)) {
                return cached.response.clone();
            }
        }
    }
    let rows = std::fs::File::open(&path)
        .ok()
        .map(|file| read(std::io::BufReader::new(file), provider, &query))
        .unwrap_or_default();
    let response = response_json(
        200,
        "OK",
        &serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()),
    );
    if query.chart && response.len() <= 2 * 1024 * 1024 {
        if let Some((len, modified)) = stamp {
            let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= 8 {
                cache.clear();
            }
            cache.insert(
                key,
                Cached {
                    len,
                    modified,
                    response: response.clone(),
                },
            );
        }
    }
    response
}

// Project only chart fields while deserializing. A snapshot may embed hundreds
// of historical chart points; allocating them just to discard them is costly.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChartRecord {
    ts: String,
    #[serde(alias = "provider_id")]
    provider_id: String,
    #[serde(alias = "display_name")]
    display_name: String,
    plan: Option<String>,
    #[serde(alias = "primary_percent")]
    primary_percent: f64,
    #[serde(alias = "input_tokens")]
    input_tokens: Option<u64>,
    #[serde(alias = "output_tokens")]
    output_tokens: Option<u64>,
    #[serde(alias = "cache_read_tokens")]
    cache_read_tokens: Option<u64>,
    #[serde(alias = "cache_creation_tokens")]
    cache_creation_tokens: Option<u64>,
    #[serde(alias = "total_tokens")]
    total_tokens: Option<u64>,
    cost: Option<f64>,
    #[serde(alias = "reset_time")]
    reset_time: Option<String>,
    #[serde(default)]
    progress: Vec<super::HistoryProgressRecord>,
}

impl From<ChartRecord> for SnapshotRecord {
    fn from(row: ChartRecord) -> Self {
        Self {
            ts: row.ts,
            provider_id: row.provider_id,
            display_name: row.display_name,
            plan: row.plan,
            primary_percent: row.primary_percent,
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cache_read_tokens: row.cache_read_tokens,
            cache_creation_tokens: row.cache_creation_tokens,
            total_tokens: row.total_tokens,
            cost: row.cost,
            reset_time: row.reset_time,
            progress: row.progress,
            text: Vec::new(),
            badges: Vec::new(),
            charts: Vec::new(),
        }
    }
}

fn read(reader: impl BufRead, provider: Option<&str>, query: &Query) -> Vec<SnapshotRecord> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Header {
        ts: String,
        #[serde(alias = "provider_id")]
        provider_id: String,
    }
    let mut raw = Vec::new();
    let mut groups: BTreeMap<(String, String), SnapshotRecord> = BTreeMap::new();
    for line in reader.lines().map_while(Result::ok) {
        let Ok(header) = serde_json::from_str::<Header>(&line) else {
            continue;
        };
        let Some(day) = header.ts.get(..10) else {
            continue;
        };
        if provider.is_some_and(|id| !header.provider_id.eq_ignore_ascii_case(id))
            || query.since.as_deref().is_some_and(|since| day < since)
            || query.until.as_deref().is_some_and(|until| day > until)
        {
            continue;
        }
        let row = if query.chart {
            serde_json::from_str::<ChartRecord>(&line).map(SnapshotRecord::from)
        } else {
            serde_json::from_str::<SnapshotRecord>(&line)
        };
        let Ok(row) = row else {
            continue;
        };
        let key = match query.group.as_str() {
            "hour" => row.ts.get(..13).unwrap_or(day).to_string(),
            "day" => day.to_string(),
            "month" => day[..7].to_string(),
            "week" => {
                let Ok(date) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
                    continue;
                };
                (date - chrono::Duration::days(date.weekday().num_days_from_monday() as i64))
                    .to_string()
            }
            _ => {
                raw.push(row);
                continue;
            }
        };
        groups
            .entry((row.provider_id.to_ascii_lowercase(), key))
            .and_modify(|current| merge(current, &row))
            .or_insert(row);
    }
    raw.extend(groups.into_values());
    raw.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.provider_id.cmp(&b.provider_id)));
    raw
}

fn merge(current: &mut SnapshotRecord, next: &SnapshotRecord) {
    let peak = current.primary_percent.max(next.primary_percent);
    let mut progress = BTreeMap::new();
    for quota in current.progress.iter().chain(&next.progress) {
        let percent = |q: &super::HistoryProgressRecord| {
            q.percent.unwrap_or_else(|| {
                if q.limit > 0.0 {
                    q.used / q.limit * 100.0
                } else {
                    0.0
                }
            })
        };
        progress
            .entry(quota.label.clone())
            .and_modify(|q| {
                if percent(quota) > percent(q) {
                    *q = quota.clone();
                }
            })
            .or_insert_with(|| quota.clone());
    }
    if next.ts >= current.ts {
        *current = next.clone();
    }
    current.primary_percent = peak;
    current.progress = progress.into_values().collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounds_filter_before_grouping_and_snapshots_use_last_counters_and_peak_quota() {
        let row = |day: &str, hour: u8, provider: &str, cost: f64, percent: u8| {
            json!({
            "ts":format!("{day}T{hour:02}:00:00Z"), "providerId":provider, "displayName":provider,
            "primaryPercent":percent,"cost":cost,"charts":[{"label":"large", "points":[]}],
            "progress":[{"label":"Quota","used":percent,"limit":100,"format":"percent"}]
        }).to_string()
        };
        let lines = [
            row("2026-09-01", 9, "codex", 10.0, 90),
            row("2026-09-01", 10, "codex", 2.0, 10),
            row("2026-09-02", 9, "codex", 12.0, 80),
            row("2026-09-01", 9, "claude", 99.0, 90),
        ]
        .join("\n");
        let query = Query::parse("since=2026-09-01&until=2026-09-01&group=day&view=chart").unwrap();
        let rows = read(std::io::Cursor::new(lines), Some("CODEX"), &query);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cost, Some(2.0));
        assert_eq!(rows[0].primary_percent, 90.0);
        assert_eq!(rows[0].progress[0].used, 90.0);
        assert!(rows[0].charts.is_empty());
        for query in [
            "since=2026-02-30",
            "group=garbage",
            "since=2026-09-02&until=2026-09-01",
            "since=2026-09-01&since=2026-09-01",
        ] {
            assert!(Query::parse(query).is_err());
        }
    }
}
