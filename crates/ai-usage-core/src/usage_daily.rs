use std::collections::BTreeMap;
use std::path::Path;

use chrono::{Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::paths;

const MAX_STORED_ROWS: usize = 25_000;

#[derive(Debug, Error)]
pub enum UsageDailyError {
    #[error(transparent)]
    Directory(#[from] paths::PathError),
    #[error("parse daily usage payload: {0}")]
    ParsePayload(#[from] serde_json::Error),
    #[error("read daily usage store {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("write daily usage store {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDailyRow {
    pub provider_id: String,
    pub display_name: String,
    pub date: String,
    pub source: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
    #[serde(flatten)]
    pub cost_components: UsageCostComponents,
    #[serde(default = "known_by_default")]
    pub tokens_known: bool,
    #[serde(default = "known_by_default")]
    pub cost_known: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_savings_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_as_of: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, UsageModelDaily>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    pub ingested_at: String,
}

fn known_by_default() -> bool {
    true
}

/// A complete breakdown of the row's recorded cost. Missing is not zero.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageCostComponents {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_usd: Option<f64>,
}

impl UsageCostComponents {
    pub fn zero() -> Self {
        Self {
            input_cost_usd: Some(0.0),
            cache_read_cost_usd: Some(0.0),
            cache_write_cost_usd: Some(0.0),
            output_cost_usd: Some(0.0),
        }
    }

    pub fn add(&mut self, other: &Self) {
        self.input_cost_usd = add_optional_cost(self.input_cost_usd, other.input_cost_usd);
        self.cache_read_cost_usd =
            add_optional_cost(self.cache_read_cost_usd, other.cache_read_cost_usd);
        self.cache_write_cost_usd =
            add_optional_cost(self.cache_write_cost_usd, other.cache_write_cost_usd);
        self.output_cost_usd = add_optional_cost(self.output_cost_usd, other.output_cost_usd);
    }

    fn validated(self, total: f64, known: bool) -> Self {
        let values = [
            self.input_cost_usd,
            self.cache_read_cost_usd,
            self.cache_write_cost_usd,
            self.output_cost_usd,
        ];
        if !known
            || !values
                .iter()
                .all(|value| value.is_some_and(|n| n.is_finite() && n >= 0.0))
        {
            return Self::default();
        }
        let sum: f64 = values.into_iter().flatten().sum();
        if !sum.is_finite()
            || !total.is_finite()
            || (sum - total).abs() > total.abs().max(1.0) * 1e-9
        {
            return Self::default();
        }
        self
    }
}

/// Exclusive input classes; reasoning is included in output_tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelDaily {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
    #[serde(flatten)]
    pub cost_components: UsageCostComponents,
    pub tokens_known: bool,
    pub cost_known: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_savings_usd: Option<f64>,
}

impl Default for UsageModelDaily {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            cost_usd: 0.0,
            cost_components: UsageCostComponents::zero(),
            tokens_known: true,
            cost_known: true,
            sessions: Some(0),
            cache_savings_usd: Some(0.0),
        }
    }
}

fn add_optional_cost(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    left.zip(right)
        .map(|(a, b)| a + b)
        .filter(|sum| sum.is_finite())
}

impl UsageModelDaily {
    pub fn add(&mut self, other: &Self) {
        self.tokens_known &=
            other.tokens_known && self.total_tokens.checked_add(other.total_tokens).is_some();
        self.cost_known &= other.cost_known && (self.cost_usd + other.cost_usd).is_finite();
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.reasoning_output_tokens = self
            .reasoning_output_tokens
            .saturating_add(other.reasoning_output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        if (self.cost_usd + other.cost_usd).is_finite() {
            self.cost_usd += other.cost_usd;
        }
        self.sessions = self
            .sessions
            .zip(other.sessions)
            .and_then(|(a, b)| a.checked_add(b));
        self.cache_savings_usd = add_optional_cost(self.cache_savings_usd, other.cache_savings_usd);
        self.cost_components.add(&other.cost_components);
        self.cost_components = self
            .cost_components
            .validated(self.cost_usd, self.cost_known);
    }
}

impl UsageDailyRow {
    pub fn metrics(&self) -> UsageModelDaily {
        UsageModelDaily {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            reasoning_output_tokens: self.reasoning_output_tokens,
            total_tokens: self.total_tokens,
            cost_usd: self.cost_usd,
            cost_components: self.cost_components,
            tokens_known: self.tokens_known,
            cost_known: self.cost_known,
            sessions: self.sessions,
            cache_savings_usd: self.cache_savings_usd,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredUsageDaily {
    version: u32,
    rows: Vec<UsageDailyRow>,
}

impl Default for StoredUsageDaily {
    fn default() -> Self {
        Self {
            version: 1,
            rows: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct DailyAccumulator {
    date: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
    cost_components: UsageCostComponents,
    tokens_known: bool,
    cost_known: bool,
    requests: Option<u64>,
    sessions: Option<u64>,
    cache_savings_usd: Option<f64>,
    models: BTreeMap<String, UsageModelDaily>,
}

pub fn ingest_json(provider_id: &str, payload_json: &str) -> Result<usize, UsageDailyError> {
    let payload: JsonValue = serde_json::from_str(payload_json)?;
    let new_rows = parse_rows(provider_id, &payload);
    if new_rows.is_empty() {
        return Ok(0);
    }

    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let path = paths::usage_daily_file()?;
    let mut store = read_store(&path)?;
    for row in new_rows {
        upsert_row(&mut store.rows, row);
    }
    prune_rows(&mut store.rows);
    write_store(&path, &store)?;
    Ok(store.rows.len())
}

fn parse_rows(provider_id: &str, payload: &JsonValue) -> Vec<UsageDailyRow> {
    let Some(daily) = payload.get("daily").and_then(JsonValue::as_array) else {
        return Vec::new();
    };
    if daily.is_empty() {
        return Vec::new();
    }

    let provider_id = provider_id.trim();
    if provider_id.is_empty() {
        return Vec::new();
    }

    let display_name = string_field(&payload, &["displayName", "display_name"])
        .unwrap_or_else(|| provider_id.to_string());
    let source = string_field(&payload, &["source"]).unwrap_or_else(|| "ccusage".to_string());
    let ingested_at = Utc::now().to_rfc3339();

    let mut new_rows = Vec::new();
    for entry in daily {
        let Some(date) =
            string_field(entry, &["date", "day"]).and_then(|date| normalize_day_key(&date))
        else {
            continue;
        };
        let metrics = parse_metrics(entry);
        let models = parse_models(entry);
        let requests = entry.get("requests").and_then(json_to_u64);

        new_rows.push(UsageDailyRow {
            provider_id: provider_id.to_string(),
            display_name: display_name.clone(),
            date,
            source: source.clone(),
            input_tokens: metrics.input_tokens,
            output_tokens: metrics.output_tokens,
            cache_read_tokens: metrics.cache_read_tokens,
            cache_creation_tokens: metrics.cache_creation_tokens,
            reasoning_output_tokens: metrics.reasoning_output_tokens,
            total_tokens: metrics.total_tokens,
            cost_usd: metrics.cost_usd,
            cost_components: metrics.cost_components,
            tokens_known: metrics.tokens_known,
            cost_known: metrics.cost_known,
            requests,
            sessions: metrics.sessions,
            cache_savings_usd: metrics.cache_savings_usd,
            cost_source: string_field(entry, &["costSource"])
                .or_else(|| string_field(&payload, &["costSource"])),
            pricing_as_of: string_field(entry, &["pricingAsOf"])
                .or_else(|| string_field(&payload, &["pricingAsOf"])),
            models,
            time_zone: string_field(entry, &["timeZone"])
                .or_else(|| string_field(payload, &["timeZone"])),
            ingested_at: ingested_at.clone(),
        });
    }

    new_rows
}

fn parse_metrics(entry: &JsonValue) -> UsageModelDaily {
    let input_tokens = u64_field(entry, &["inputTokens", "input_tokens"]);
    let output_tokens = u64_field(entry, &["outputTokens", "output_tokens"]);
    let cache_read_tokens = u64_field(
        entry,
        &[
            "cacheReadTokens",
            "cache_read_tokens",
            "cacheReadInputTokens",
            "cache_read_input_tokens",
            "cachedInputTokens",
            "cached_input_tokens",
        ],
    );
    let cache_creation_tokens = u64_field(
        entry,
        &[
            "cacheCreationTokens",
            "cache_creation_tokens",
            "cacheCreationInputTokens",
            "cache_creation_input_tokens",
            "cacheCreateTokens",
            "cache_create_tokens",
        ],
    );
    let reasoning_output_tokens = u64_field(
        entry,
        &[
            "reasoningOutputTokens",
            "reasoning_output_tokens",
            "reasoningTokens",
            "reasoning_tokens",
        ],
    );
    let total_tokens = u64_field(entry, &["totalTokens", "total_tokens", "tokens"]);
    // Reasoning is a subset of output, not an additional billable token class.
    let computed_total = input_tokens
        .checked_add(output_tokens)
        .and_then(|sum| sum.checked_add(cache_read_tokens))
        .and_then(|sum| sum.checked_add(cache_creation_tokens));
    let total_tokens = normalized_total(total_tokens, computed_total, reasoning_output_tokens);
    let cost_usd = f64_field(
        entry,
        &[
            "costUsd",
            "costUSD",
            "cost_usd",
            "totalCost",
            "total_cost",
            "cost",
        ],
    );
    let tokens_known = computed_total.is_some()
        && entry
            .get("tokensKnown")
            .and_then(JsonValue::as_bool)
            .unwrap_or_else(|| {
                [
                    "totalTokens",
                    "total_tokens",
                    "tokens",
                    "inputTokens",
                    "input_tokens",
                    "outputTokens",
                    "output_tokens",
                    "cacheReadTokens",
                    "cache_read_tokens",
                    "cacheCreationTokens",
                    "cache_creation_tokens",
                    "cacheReadInputTokens",
                    "cacheCreationInputTokens",
                ]
                .iter()
                .any(|key| entry.get(*key).and_then(json_to_u64).is_some())
            });
    let cost_known = entry
        .get("costKnown")
        .and_then(JsonValue::as_bool)
        .unwrap_or_else(|| {
            [
                "costUsd",
                "costUSD",
                "cost_usd",
                "totalCost",
                "total_cost",
                "cost",
            ]
            .iter()
            .any(|key| entry.get(*key).and_then(json_to_f64).is_some())
        });
    UsageModelDaily {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        reasoning_output_tokens,
        total_tokens,
        cost_usd,
        cost_components: UsageCostComponents {
            input_cost_usd: entry.get("inputCostUsd").and_then(json_to_f64),
            cache_read_cost_usd: entry.get("cacheReadCostUsd").and_then(json_to_f64),
            cache_write_cost_usd: entry.get("cacheWriteCostUsd").and_then(json_to_f64),
            output_cost_usd: entry.get("outputCostUsd").and_then(json_to_f64),
        }
        .validated(cost_usd, cost_known),
        tokens_known,
        cost_known,
        sessions: entry
            .get("sessions")
            .or_else(|| entry.get("sessionCount"))
            .and_then(json_to_u64),
        cache_savings_usd: entry.get("cacheSavingsUsd").and_then(json_to_f64),
    }
}

fn parse_models(entry: &JsonValue) -> BTreeMap<String, UsageModelDaily> {
    if let Some(models) = entry.get("models").and_then(JsonValue::as_object) {
        return models
            .iter()
            .filter(|(name, value)| !name.trim().is_empty() && value.is_object())
            .map(|(name, value)| (name.clone(), parse_metrics(value)))
            .collect();
    }
    entry
        .get("modelBreakdowns")
        .or_else(|| entry.get("model_breakdowns"))
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            string_field(value, &["modelName", "model_name", "model", "name"])
                .map(|name| (name, parse_metrics(value)))
        })
        .collect()
}

fn normalized_total(reported: u64, components: Option<u64>, reasoning: u64) -> u64 {
    // Repair the identifiable legacy double-count, while retaining a reported
    // total when some token classes were not supplied.
    let Some(components) = components else {
        return u64::MAX;
    };
    if reasoning > 0 && components.checked_add(reasoning) == Some(reported) {
        components
    } else {
        reported.max(components)
    }
}

pub fn report_json(provider_id: &str, report: &str) -> Result<JsonValue, UsageDailyError> {
    let rows = selected_daily_rows(provider_id)?;
    if rows.is_empty() {
        return Ok(serde_json::json!({
            "error": {
                "code": "UNAVAILABLE",
                "message": "Saved daily usage is not available for this provider"
            }
        }));
    }

    match report {
        "daily" => Ok(serde_json::json!({ "daily": rows })),
        "weekly" => Ok(serde_json::json!({ "weekly": aggregate_rows(rows, Bucket::Week) })),
        "monthly" => Ok(serde_json::json!({ "monthly": aggregate_rows(rows, Bucket::Month) })),
        _ => Ok(serde_json::json!({
            "error": {
                "code": "BAD_REPORT",
                "message": "Saved daily usage only supports daily, weekly, and monthly reports"
            }
        })),
    }
}

pub fn selected_daily_rows(provider_id: &str) -> Result<Vec<UsageDailyRow>, UsageDailyError> {
    selected_rows(provider_id, false)
}

/// Select whole rows from a single source per day. Never attach a lower-priority
/// source's model estimates to billing totals and imply they reconcile.
pub fn selected_model_daily_rows(provider_id: &str) -> Result<Vec<UsageDailyRow>, UsageDailyError> {
    selected_rows(provider_id, true)
}

fn selected_rows(
    provider_id: &str,
    require_models: bool,
) -> Result<Vec<UsageDailyRow>, UsageDailyError> {
    let provider_id = provider_id.trim();
    if provider_id.is_empty() {
        return Ok(Vec::new());
    }

    let mut by_day: BTreeMap<String, UsageDailyRow> = BTreeMap::new();
    for row in read_store(&paths::usage_daily_file()?)?.rows {
        if !row.provider_id.eq_ignore_ascii_case(provider_id)
            || (require_models && row.models.is_empty())
        {
            continue;
        }
        match by_day.get(&row.date) {
            Some(existing) if !prefer_row(&row, existing) => {}
            _ => {
                by_day.insert(row.date.clone(), row);
            }
        }
    }
    Ok(by_day.into_values().collect())
}

pub fn all_selected_daily_rows() -> Result<Vec<UsageDailyRow>, UsageDailyError> {
    let mut by_provider_day: BTreeMap<(String, String), UsageDailyRow> = BTreeMap::new();
    for row in read_store(&paths::usage_daily_file()?)?.rows {
        let key = (row.provider_id.to_ascii_lowercase(), row.date.clone());
        match by_provider_day.get(&key) {
            Some(existing) if !prefer_row(&row, existing) => {}
            _ => {
                by_provider_day.insert(key, row);
            }
        }
    }
    Ok(by_provider_day.into_values().collect())
}

/// A response-only view; source summaries never enter the persisted accounting rows.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDailyWithSources {
    #[serde(flatten)]
    pub row: UsageDailyRow,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source_rows: Vec<UsageDailySource>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDailySource {
    pub source: String,
    pub selected: bool,
    pub ingested_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_as_of: Option<String>,
    pub cost_usd: f64,
    pub cost_known: bool,
    pub total_tokens: u64,
    pub tokens_known: bool,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
}

/// Read the store once so the selected row and its alternatives share a snapshot.
/// No transcript scans, model expansion, or repricing are performed here.
pub fn selected_daily_rows_with_sources(
    provider_id: Option<&str>,
) -> Result<Vec<UsageDailyWithSources>, UsageDailyError> {
    let rows = read_store(&paths::usage_daily_file()?)?.rows;
    Ok(daily_rows_with_sources(rows, provider_id))
}

fn daily_rows_with_sources(
    rows: Vec<UsageDailyRow>,
    provider_id: Option<&str>,
) -> Vec<UsageDailyWithSources> {
    let mut by_day: BTreeMap<(String, String), Vec<UsageDailyRow>> = BTreeMap::new();
    for row in rows {
        if provider_id.is_some_and(|id| !row.provider_id.eq_ignore_ascii_case(id.trim())) {
            continue;
        }
        by_day
            .entry((row.provider_id.to_ascii_lowercase(), row.date.clone()))
            .or_default()
            .push(row);
    }
    by_day
        .into_values()
        .map(|rows| {
            let mut selected = 0;
            let mut by_source: BTreeMap<&str, &UsageDailyRow> = BTreeMap::new();
            for (index, row) in rows.iter().enumerate() {
                if prefer_row(row, &rows[selected]) {
                    selected = index;
                }
                match by_source.get(row.source.as_str()) {
                    Some(existing) if !prefer_row(row, existing) => {}
                    _ => {
                        by_source.insert(&row.source, row);
                    }
                }
            }
            let source_rows = if by_source.len() > 1 {
                by_source
                    .into_values()
                    .map(|row| UsageDailySource {
                        selected: row.source == rows[selected].source,
                        ingested_at: row.ingested_at.clone(),
                        time_zone: row.time_zone.clone(),
                        source: row.source.clone(),
                        cost_source: row.cost_source.clone(),
                        pricing_as_of: row.pricing_as_of.clone(),
                        cost_usd: row.cost_usd,
                        cost_known: row.cost_known,
                        total_tokens: row.total_tokens,
                        tokens_known: row.tokens_known,
                        input_tokens: row.input_tokens,
                        cache_read_tokens: row.cache_read_tokens,
                        cache_creation_tokens: row.cache_creation_tokens,
                        output_tokens: row.output_tokens,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            UsageDailyWithSources {
                row: rows.into_iter().nth(selected).unwrap(),
                source_rows,
            }
        })
        .collect()
}

/// Calendar-day reporting uses UTC consistently across saved rows, CLI and dashboard.
pub fn period_start(period: &str, today: NaiveDate) -> Result<Option<NaiveDate>, &'static str> {
    match period {
        "all" => Ok(None),
        "month" => Ok(today.with_day(1)),
        value => {
            let days = value
                .parse::<i64>()
                .ok()
                .filter(|days| (1..=36600).contains(days))
                .ok_or("Period must be month, all, or 1-36600 days")?;
            Ok(today.checked_sub_signed(Duration::days(days - 1)))
        }
    }
}

pub fn filter_period(
    rows: &mut Vec<UsageDailyRow>,
    period: &str,
    today: NaiveDate,
) -> Result<(), &'static str> {
    let start = period_start(period, today)?;
    rows.retain(|row| {
        NaiveDate::parse_from_str(&row.date, "%Y-%m-%d")
            .ok()
            .is_some_and(|day| day <= today && start.is_none_or(|start| day >= start))
    });
    Ok(())
}

fn read_store(path: &Path) -> Result<StoredUsageDaily, UsageDailyError> {
    if !path.exists() {
        return Ok(StoredUsageDaily::default());
    }
    let text = std::fs::read_to_string(path).map_err(|source| UsageDailyError::Read {
        path: path.display().to_string(),
        source,
    })?;
    if text.trim().is_empty() {
        return Ok(StoredUsageDaily::default());
    }
    if let Ok(mut store) = serde_json::from_str::<StoredUsageDaily>(&text) {
        for row in &mut store.rows {
            normalize_stored_total(row);
        }
        return Ok(store);
    }
    let mut rows: Vec<UsageDailyRow> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<UsageDailyRow>(line).ok())
        .collect();
    for row in &mut rows {
        normalize_stored_total(row);
    }
    Ok(StoredUsageDaily { version: 1, rows })
}

fn normalize_stored_total(row: &mut UsageDailyRow) {
    let sum = row
        .input_tokens
        .checked_add(row.output_tokens)
        .and_then(|n| n.checked_add(row.cache_read_tokens))
        .and_then(|n| n.checked_add(row.cache_creation_tokens));
    row.total_tokens = normalized_total(row.total_tokens, sum, row.reasoning_output_tokens);
    row.tokens_known &= sum.is_some();
    row.cost_components = row.cost_components.validated(row.cost_usd, row.cost_known);
    for model in row.models.values_mut() {
        model.cost_components = model
            .cost_components
            .validated(model.cost_usd, model.cost_known);
    }
}

fn write_store(path: &Path, store: &StoredUsageDaily) -> Result<(), UsageDailyError> {
    let mut json = serde_json::to_vec_pretty(store).map_err(UsageDailyError::ParsePayload)?;
    json.push(b'\n');
    if std::fs::read(path).ok().as_deref() == Some(json.as_slice()) {
        return Ok(());
    }
    crate::storage::write_atomic(path, &json).map_err(|source| UsageDailyError::Write {
        path: path.display().to_string(),
        source,
    })
}

fn upsert_row(rows: &mut Vec<UsageDailyRow>, mut row: UsageDailyRow) {
    if let Some(existing) = rows.iter_mut().find(|existing| {
        existing.provider_id.eq_ignore_ascii_case(&row.provider_id)
            && existing.date == row.date
            && existing.source == row.source
    }) {
        let updated_at = row.ingested_at.clone();
        row.ingested_at = existing.ingested_at.clone();
        if serde_json::to_value(&row).ok() != serde_json::to_value(&*existing).ok() {
            row.ingested_at = updated_at;
            *existing = row;
        }
    } else {
        rows.push(row);
    }
}

fn prune_rows(rows: &mut Vec<UsageDailyRow>) {
    rows.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(a.provider_id.cmp(&b.provider_id))
            .then(a.source.cmp(&b.source))
    });
    if rows.len() > MAX_STORED_ROWS {
        let remove = rows.len() - MAX_STORED_ROWS;
        rows.drain(0..remove);
    }
}

fn prefer_row(candidate: &UsageDailyRow, existing: &UsageDailyRow) -> bool {
    let candidate_priority = source_priority(&candidate.source);
    let existing_priority = source_priority(&existing.source);
    candidate_priority > existing_priority
        || (candidate_priority == existing_priority && candidate.ingested_at > existing.ingested_at)
}

fn source_priority(source: &str) -> u8 {
    let source = source.to_ascii_lowercase();
    if source.contains("billing") {
        40
    } else if source.contains("transcript") {
        30
    } else if source.contains("ccusage") {
        20
    } else {
        10
    }
}

#[derive(Clone, Copy)]
enum Bucket {
    Week,
    Month,
}

fn aggregate_rows(rows: Vec<UsageDailyRow>, bucket: Bucket) -> Vec<UsageDailyRow> {
    let mut map: BTreeMap<String, DailyAccumulator> = BTreeMap::new();
    for row in rows {
        let key = match bucket {
            Bucket::Week => week_key(&row.date).unwrap_or_else(|| row.date.clone()),
            Bucket::Month => row.date.get(0..7).unwrap_or(&row.date).to_string(),
        };
        let entry = map.entry(key.clone()).or_insert_with(|| DailyAccumulator {
            date: key,
            tokens_known: true,
            cost_known: true,
            requests: Some(0),
            sessions: Some(0),
            cache_savings_usd: Some(0.0),
            cost_components: UsageCostComponents::zero(),
            ..DailyAccumulator::default()
        });
        entry.tokens_known &=
            row.tokens_known && entry.total_tokens.checked_add(row.total_tokens).is_some();
        entry.cost_known &= row.cost_known && (entry.cost_usd + row.cost_usd).is_finite();
        entry.input_tokens = entry.input_tokens.saturating_add(row.input_tokens);
        entry.output_tokens = entry.output_tokens.saturating_add(row.output_tokens);
        entry.cache_read_tokens = entry
            .cache_read_tokens
            .saturating_add(row.cache_read_tokens);
        entry.cache_creation_tokens = entry
            .cache_creation_tokens
            .saturating_add(row.cache_creation_tokens);
        entry.reasoning_output_tokens = entry
            .reasoning_output_tokens
            .saturating_add(row.reasoning_output_tokens);
        entry.total_tokens = entry.total_tokens.saturating_add(row.total_tokens);
        if (entry.cost_usd + row.cost_usd).is_finite() {
            entry.cost_usd += row.cost_usd;
        }
        entry.requests = entry
            .requests
            .zip(row.requests)
            .and_then(|(a, b)| a.checked_add(b));
        // This is a count of session-days, not distinct sessions across the bucket.
        entry.sessions = entry
            .sessions
            .zip(row.sessions)
            .and_then(|(a, b)| a.checked_add(b));
        entry.cache_savings_usd = add_optional_cost(entry.cache_savings_usd, row.cache_savings_usd);
        entry.cost_components.add(&row.cost_components);
        entry.cost_components = entry
            .cost_components
            .validated(entry.cost_usd, entry.cost_known);
        for (name, model) in row.models {
            entry
                .models
                .entry(name)
                .and_modify(|sum| sum.add(&model))
                .or_insert(model);
        }
    }

    map.into_values()
        .map(|row| UsageDailyRow {
            provider_id: String::new(),
            display_name: String::new(),
            date: row.date,
            source: "saved_daily".to_string(),
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cache_read_tokens: row.cache_read_tokens,
            cache_creation_tokens: row.cache_creation_tokens,
            reasoning_output_tokens: row.reasoning_output_tokens,
            total_tokens: row.total_tokens,
            cost_usd: row.cost_usd,
            cost_components: row.cost_components,
            cost_known: row.cost_known,
            tokens_known: row.tokens_known,
            requests: row.requests,
            sessions: row.sessions,
            cache_savings_usd: row.cache_savings_usd,
            cost_source: None,
            pricing_as_of: None,
            models: row.models,
            time_zone: None,
            ingested_at: String::new(),
        })
        .collect()
}

fn week_key(date: &str) -> Option<String> {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let week_start = date - Duration::days(i64::from(date.weekday().num_days_from_monday()));
    Some(week_start.format("%Y-%m-%d").to_string())
}

fn normalize_day_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.len() >= 10
        && trimmed.as_bytes().get(4) == Some(&b'-')
        && trimmed.as_bytes().get(7) == Some(&b'-')
    {
        let date = trimmed.get(..10)?;
        return NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .ok()
            .map(|date| date.to_string());
    }
    let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        let formatted = format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8]);
        return NaiveDate::parse_from_str(&formatted, "%Y-%m-%d")
            .ok()
            .map(|date| date.to_string());
    }
    None
}

fn string_field(value: &JsonValue, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn u64_field(value: &JsonValue, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(json_to_u64)
        .unwrap_or(0)
}

fn f64_field(value: &JsonValue, keys: &[&str]) -> f64 {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(json_to_f64)
        .unwrap_or(0.0)
}

fn json_to_u64(value: &JsonValue) -> Option<u64> {
    if let Some(value) = value.as_u64() {
        return Some(value);
    }
    if let Some(value) = value.as_i64() {
        return (value >= 0).then_some(value as u64);
    }
    value
        .as_str()
        .and_then(|value| value.trim().replace(',', "").parse::<u64>().ok())
}

fn json_to_f64(value: &JsonValue) -> Option<f64> {
    let parsed = value.as_f64().or_else(|| {
        value
            .as_str()
            .and_then(|value| value.trim().replace(['$', ','], "").parse::<f64>().ok())
    })?;
    (parsed.is_finite() && parsed >= 0.0).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_costs_survive_storage_and_grouping_without_inventing_missing_costs() {
        let priced = serde_json::json!({"date":"2026-09-01", "costUsd":1.0,
            "inputCostUsd":0.2, "cacheReadCostUsd":0.1, "cacheWriteCostUsd":0.0, "outputCostUsd":0.7});
        let mut payload = serde_json::json!({"daily":[priced.clone()]});
        payload["daily"][0]["models"] = serde_json::json!({"priced":priced});
        let row = parse_rows("codex", &payload).remove(0);
        let stored: UsageDailyRow =
            serde_json::from_value(serde_json::to_value(&row).unwrap()).unwrap();
        assert_eq!(stored.cost_components, row.cost_components);
        assert_eq!(stored.models["priced"].cost_components, row.cost_components);
        assert_eq!(stored.cost_components.cache_write_cost_usd, Some(0.0));
        let grouped = aggregate_rows(vec![row.clone(), row.clone()], Bucket::Week);
        assert_eq!(grouped[0].cost_components.input_cost_usd, Some(0.4));
        assert_eq!(grouped[0].cost_components.output_cost_usd, Some(1.4));
        assert_eq!(
            grouped[0].models["priced"].cost_components,
            grouped[0].cost_components
        );
        let mut missing = row.clone();
        missing.cost_components = UsageCostComponents::default();
        let grouped = aggregate_rows(vec![row, missing], Bucket::Month);
        assert_eq!(grouped[0].cost_usd, 2.0);
        assert_eq!(grouped[0].cost_components, UsageCostComponents::default());
        assert!(
            serde_json::to_value(&grouped[0])
                .unwrap()
                .get("inputCostUsd")
                .is_none()
        );
    }

    #[test]
    fn component_costs_require_a_complete_matching_known_total() {
        let valid = serde_json::json!({"costUsd":1.0, "inputCostUsd":0.2,
            "cacheReadCostUsd":0.1, "cacheWriteCostUsd":0.0, "outputCostUsd":0.7});
        for (field, value) in [
            ("costKnown", serde_json::json!(false)),
            ("costUsd", serde_json::json!(2.0)),
            ("inputCostUsd", serde_json::json!(-1)),
            ("cacheReadCostUsd", serde_json::Value::Null),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert_eq!(
                parse_metrics(&invalid).cost_components,
                UsageCostComponents::default(),
                "{field}"
            );
        }
        let legacy = parse_metrics(&serde_json::json!({"costUsd":0.0}));
        assert_eq!(legacy.cost_components, UsageCostComponents::default());
        let zero = parse_metrics(&serde_json::json!({"costUsd":0.0, "inputCostUsd":0.0,
            "cacheReadCostUsd":0.0, "cacheWriteCostUsd":0.0, "outputCostUsd":0.0}));
        assert_eq!(zero.cost_components, UsageCostComponents::zero());
        let mut sum = parse_metrics(&valid);
        sum.add(&parse_metrics(&valid));
        assert_eq!(sum.cost_components.output_cost_usd, Some(1.4));
        sum.add(&legacy);
        assert!(sum.cost_known);
        assert_eq!(sum.cost_components, UsageCostComponents::default());
    }

    #[test]
    fn ingestion_retains_models_sessions_savings_and_explicit_unknown_prices() {
        let payload = serde_json::json!({"displayName":"Codex", "source":"local-transcript-v2", "costSource":"api-rate-estimate", "pricingAsOf":"2026-09-27",
        "daily":[{"date":"2026-09-01", "inputTokens":10,"outputTokens":5,"cacheReadTokens":20,
            "reasoningOutputTokens":3,"totalTokens":38,"costUsd":1.0,"costKnown":false,"sessions":2,
            "models":{
                "known":{"inputTokens":10,"outputTokens":5,"cacheReadTokens":10,"reasoningOutputTokens":3,"costUsd":1.0,"sessions":1,"cacheSavingsUsd":0.25},
                "unknown":{"cacheReadTokens":10,"costUsd":0,"costKnown":false,"sessions":1}
            }}]});
        let rows = parse_rows("codex", &payload);
        let row = &rows[0];
        assert_eq!(row.total_tokens, 35);
        assert_eq!(row.sessions, Some(2));
        assert_eq!(row.cost_source.as_deref(), Some("api-rate-estimate"));
        assert_eq!(row.pricing_as_of.as_deref(), Some("2026-09-27"));
        assert!(!row.cost_known);
        assert_eq!(row.models["known"].total_tokens, 25);
        assert_eq!(row.models["known"].cache_savings_usd, Some(0.25));
        assert!(!row.models["unknown"].cost_known);
        let stored: UsageDailyRow =
            serde_json::from_value(serde_json::to_value(row).unwrap()).unwrap();
        assert_eq!(stored.models, row.models);
        let grouped = aggregate_rows(vec![row.clone(), row.clone()], Bucket::Week);
        assert_eq!(grouped[0].models["known"].total_tokens, 50);
        assert_eq!(grouped[0].models["known"].sessions, Some(2));
        assert!(!grouped[0].models["unknown"].cost_known);
    }

    #[test]
    fn model_breakdown_arrays_and_partial_token_totals_preserve_missingness() {
        let rows = parse_rows(
            "claude",
            &serde_json::json!({"daily":[{"date":"2026-09-01","totalTokens":100,"costUsd":10,
            "modelBreakdowns":[{"modelName":"a","totalTokens":60,"inputTokens":0,"outputTokens":0},
                {"modelName":"b","totalTokens":40,"costUsd":0,"costKnown":true}]}]}),
        );
        assert_eq!(rows[0].models["a"].total_tokens, 60);
        assert!(!rows[0].models["a"].cost_known);
        assert!(rows[0].models["b"].cost_known);
        assert_eq!(rows[0].sessions, None);
        assert_eq!(rows[0].models["a"].sessions, None);
        assert_eq!(rows[0].cache_savings_usd, None);
    }

    #[test]
    fn ingestion_preserves_explicit_zone_without_backfilling_legacy_rows() {
        let payload = serde_json::json!({"timeZone":"Europe/Amsterdam","daily":[
            {"date":"2026-09-27","totalTokens":10,"costUsd":1},
            {"date":"2026-09-26","totalTokens":20,"costUsd":2,"timeZone":"UTC"}
        ]});
        let rows = parse_rows("codex", &payload);
        assert_eq!(rows[0].time_zone.as_deref(), Some("Europe/Amsterdam"));
        assert_eq!(rows[1].time_zone.as_deref(), Some("UTC"));
        let mut legacy = serde_json::to_value(&rows[0]).unwrap();
        legacy.as_object_mut().unwrap().remove("timeZone");
        let restored: UsageDailyRow = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.time_zone, None);
        assert!(serde_json::to_value(&restored).unwrap().get("timeZone").is_none());
        let mut local = rows[0].clone();
        local.source = "local-transcript-v2".into();
        local.time_zone = Some("UTC".into());
        let view = daily_rows_with_sources(vec![restored, local], None);
        assert!(view[0].source_rows.iter().all(|source| !source.ingested_at.is_empty()));
        assert_eq!(view[0].source_rows.iter().find(|source| source.selected).unwrap().time_zone.as_deref(), Some("UTC"));
        assert_eq!(view[0].source_rows.iter().find(|source| !source.selected).unwrap().time_zone, None);
    }

    #[test]
    fn source_summaries_deduplicate_retained_sources_without_changing_selection() {
        let mut older = row("2026-09-01");
        older.source = "ccusage".into();
        older.ingested_at = "2026-09-01".into();
        let mut newer = older.clone();
        newer.ingested_at = "2026-09-02".into();
        newer.cost_usd = 2.0;
        let mut billing = older.clone();
        billing.source = "billing".into();
        billing.cost_usd = 3.0;
        let single = row("2026-09-02");
        let rows = daily_rows_with_sources(vec![older, newer, billing, single], Some("PI"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].row.source, "billing");
        assert_eq!(rows[0].source_rows.len(), 2);
        let ccusage = rows[0].source_rows.iter().find(|row| row.source == "ccusage").unwrap();
        assert_eq!(ccusage.cost_usd, 2.0);
        assert!(!ccusage.selected);
        assert_eq!(rows[0].source_rows.iter().filter(|row| row.selected).count(), 1);
        assert!(rows[1].source_rows.is_empty());
        assert!(serde_json::to_value(&rows[1]).unwrap().get("sourceRows").is_none());
        assert!(daily_rows_with_sources(vec![row("2026-09-01")], Some("other")).is_empty());
    }

    #[test]
    fn billing_then_local_transcripts_then_ccusage_select_whole_rows() {
        let mut ccusage = row("2026-09-01");
        ccusage.source = "ccusage".into();
        ccusage.ingested_at = "2026-09-28".into();
        ccusage.cost_usd = 99.0;
        let mut local = ccusage.clone();
        local.source = "local-transcript-v2".into();
        local.ingested_at = "2026-09-27".into();
        local.cost_usd = 1.0;
        local.cost_known = false;
        let mut billing = ccusage.clone();
        billing.source = "billing".into();
        billing.ingested_at = "2026-09-26".into();
        billing.cost_usd = 5.0;
        assert!(prefer_row(&billing, &local));
        assert!(prefer_row(&local, &ccusage), "a newer or fully priced ccusage row must not displace local logs");
        let mut billed_local = local.clone();
        billed_local.date = "2026-09-02".into();
        billing.date = billed_local.date.clone();
        let mut fallback = ccusage.clone();
        fallback.date = "2026-09-03".into();
        let rows = daily_rows_with_sources(vec![ccusage, local, billed_local, billing, fallback], None);
        assert_eq!(rows.iter().map(|row| row.row.source.as_str()).collect::<Vec<_>>(), vec!["local-transcript-v2", "billing", "ccusage"]);
        assert_eq!(rows[0].row.cost_usd, 1.0);
        assert!(!rows[0].row.cost_known);
        assert_eq!(rows[0].source_rows.len(), 2, "non-selected ccusage provenance is retained");
        assert!(rows[0].source_rows.iter().find(|row| row.source == "local-transcript-v2").unwrap().selected);
        assert!(!rows[0].source_rows.iter().find(|row| row.source == "ccusage").unwrap().selected);
        assert!(rows[2].source_rows.is_empty(), "ccusage-only day remains available");
    }

    #[test]
    fn legacy_rows_stay_readable_and_source_priority_keeps_billing_authoritative() {
        let mut legacy = serde_json::to_value(row("2026-09-01")).unwrap();
        for key in [
            "models",
            "sessions",
            "cacheSavingsUsd",
            "costSource",
            "pricingAsOf",
        ] {
            legacy.as_object_mut().unwrap().remove(key);
        }
        legacy["totalTokens"] = serde_json::json!(18);
        let mut row: UsageDailyRow = serde_json::from_value(legacy).unwrap();
        normalize_stored_total(&mut row);
        assert_eq!(row.total_tokens, 15);
        assert!(row.models.is_empty());
        assert_eq!(row.sessions, None);
        let mut billing = row.clone();
        billing.source = "billing".into();
        row.source = "local-transcript-v2".into();
        row.ingested_at = "later".into();
        assert!(!prefer_row(&row, &billing));
    }

    fn row(date: &str) -> UsageDailyRow {
        UsageDailyRow {
            provider_id: "pi".into(),
            display_name: "Pi".into(),
            date: date.into(),
            source: "transcript".into(),
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            reasoning_output_tokens: 3,
            total_tokens: 15,
            cost_usd: 0.1,
            cost_components: UsageCostComponents::default(),
            tokens_known: true,
            cost_known: true,
            requests: Some(1),
            sessions: None,
            cache_savings_usd: None,
            cost_source: None,
            pricing_as_of: None,
            models: BTreeMap::new(),
            time_zone: None,
            ingested_at: "first".into(),
        }
    }

    #[test]
    fn incomplete_totals_remain_incomplete_independently() {
        let mut missing_cost = row("2026-09-02");
        missing_cost.cost_known = false;
        missing_cost.cost_usd = 0.0;
        let result = aggregate_rows(vec![row("2026-09-01"), missing_cost], Bucket::Month);
        assert_eq!(result[0].total_tokens, 30);
        assert_eq!(result[0].cost_usd, 0.1);
        assert!(result[0].tokens_known);
        assert!(!result[0].cost_known);
        assert_eq!(result[0].requests, Some(2));
    }

    #[test]
    fn overflow_keeps_finite_partial_amounts() {
        let mut large = row("2026-09-01");
        large.cost_usd = f64::MAX;
        large.total_tokens = u64::MAX;
        let result = aggregate_rows(vec![large.clone(), large], Bucket::Month);
        assert!(!result[0].tokens_known && !result[0].cost_known);
        assert!(result[0].cost_usd.is_finite());
        assert!(serde_json::to_string(&result).is_ok());
    }

    #[test]
    fn repeated_identical_reports_do_not_change_ingestion_time() {
        let mut rows = vec![row("2026-09-01")];
        let mut repeated = rows[0].clone();
        repeated.ingested_at = "second".into();
        upsert_row(&mut rows, repeated.clone());
        assert_eq!(rows[0].ingested_at, "first");
        repeated.cost_known = false;
        upsert_row(&mut rows, repeated);
        assert_eq!(rows[0].ingested_at, "second");
    }

    #[test]
    fn validates_dates_and_missing_numbers() {
        assert_eq!(normalize_day_key("20260927"), Some("2026-09-27".into()));
        assert_eq!(normalize_day_key("2026-02-30"), None);
        assert_eq!(normalize_day_key("2026-09-\u{00e9}"), None);
        assert_eq!(json_to_f64(&serde_json::json!(null)), None);
        assert_eq!(json_to_f64(&serde_json::json!("NaN")), None);
        assert_eq!(json_to_f64(&serde_json::json!(-1)), None);
        assert_eq!(json_to_f64(&serde_json::json!(0)), Some(0.0));
    }

    #[test]
    fn periods_are_inclusive_and_reject_future_or_invalid_days() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 27).unwrap();
        assert_eq!(
            period_start("30", today).unwrap().unwrap().to_string(),
            "2026-08-29"
        );
        let mut rows = vec![
            row("2026-08-31"),
            row("2026-09-01"),
            row("2026-09-27"),
            row("2026-09-28"),
        ];
        filter_period(&mut rows, "month", today).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(period_start("0", today).is_err());
        assert!(period_start("9999999999", today).is_err());
    }
}
