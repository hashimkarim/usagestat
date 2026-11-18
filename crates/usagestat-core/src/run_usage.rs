//! Account-scoped execution reports received by the existing Usagestat backend.
//! Provider quota snapshots and daily imports remain independent observations.
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Datelike, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const EVENT_SCHEMA: &str = "agenticdriver.usage.v2";
pub const MAX_EVENT_BYTES: usize = 65_536;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Error)]
pub enum RunUsageError {
    #[error("invalid execution usage record")]
    InvalidRecord,
    #[error("execution usage record has expired")]
    Expired,
    #[error("the event identity already belongs to a different record")]
    Conflict,
    #[error("the run usage store has reached its configured capacity")]
    Capacity,
    #[error("invalid run usage storage policy")]
    InvalidPolicy,
    #[error("undelivered records are bound to a different forwarding destination")]
    TargetConflict,
    #[error("run usage storage requires a private directory and regular database file")]
    Permissions,
    #[error("run usage storage is unavailable")]
    Storage,
}

impl From<rusqlite::Error> for RunUsageError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}

// Optional counters are absent when unknown; explicit null is not an absent counter.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(de: D) -> Result<Option<T>, D::Error> {
    T::deserialize(de).map(Some)
}
fn count<'de, D: Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    let value = f64::deserialize(de)?;
    if !value.is_finite() || value < 0.0 || value > MAX_SAFE_INTEGER as f64 || value.fract() != 0.0
    {
        return Err(serde::de::Error::custom(
            "counter must be a nonnegative safe integer",
        ));
    }
    Ok(value as u64)
}
fn optional_count<'de, D: Deserializer<'de>>(de: D) -> Result<Option<u64>, D::Error> {
    count(de).map(Some)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Measurements {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub input_tokens: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub output_tokens: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub cached_input_tokens: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub reasoning_tokens: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub cost_usd: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub api_equivalent_cost_usd: Option<f64>,
}

impl Measurements {
    fn values(&self) -> [Option<f64>; 6] {
        [
            self.input_tokens.map(|v| v as f64),
            self.output_tokens.map(|v| v as f64),
            self.cached_input_tokens.map(|v| v as f64),
            self.reasoning_tokens.map(|v| v as f64),
            self.cost_usd,
            self.api_equivalent_cost_usd,
        ]
    }
    fn valid(&self) -> bool {
        [
            self.input_tokens,
            self.output_tokens,
            self.cached_input_tokens,
            self.reasoning_tokens,
        ]
        .into_iter()
        .flatten()
        .all(|value| value <= MAX_SAFE_INTEGER)
            && [self.cost_usd, self.api_equivalent_cost_usd]
                .into_iter()
                .flatten()
                .all(|value| value.is_finite() && value >= 0.0)
    }
    fn valid_subsets(&self) -> bool {
        !matches!((self.input_tokens, self.cached_input_tokens), (Some(total), Some(subset)) if subset > total)
            && !matches!((self.output_tokens, self.reasoning_tokens), (Some(total), Some(subset)) if subset > total)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportedSteps {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub input_tokens: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub output_tokens: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub cached_input_tokens: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub reasoning_tokens: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub cost_usd: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_count"
    )]
    pub api_equivalent_cost_usd: Option<u64>,
}
impl ReportedSteps {
    fn values(&self) -> [Option<u64>; 6] {
        [
            self.input_tokens,
            self.output_tokens,
            self.cached_input_tokens,
            self.reasoning_tokens,
            self.cost_usd,
            self.api_equivalent_cost_usd,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Coverage {
    #[serde(deserialize_with = "count")]
    pub started_steps: u64,
    #[serde(deserialize_with = "count")]
    pub completed_steps: u64,
    pub reported_steps: ReportedSteps,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunUsageRecord {
    pub schema: String,
    pub event_id: String,
    pub run_id: String,
    pub host_id: String,
    /// Durable ingestion requires an explicit account; unbound SDK reports stay local.
    pub account_id: String,
    pub subject: String,
    pub provider: String,
    pub vendor: String,
    pub model: String,
    pub auth_mode: String,
    pub status: String,
    pub source: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub expires_at: Option<DateTime<Utc>>,
    pub usage: Measurements,
    pub observed_usage: Measurements,
    pub coverage: Coverage,
    #[serde(deserialize_with = "count")]
    pub duration_ms: u64,
    pub metadata: BTreeMap<String, String>,
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn valid_uuid(value: &str) -> bool {
    if matches!(
        value,
        "00000000-0000-0000-0000-000000000000" | "ffffffff-ffff-ffff-ffff-ffffffffffff"
    ) {
        return true;
    }
    let parts: Vec<_> = value.split('-').collect();
    parts.len() == 5
        && parts.iter().zip([8, 4, 4, 4, 12]).all(|(part, length)| {
            part.len() == length && part.bytes().all(|b| b.is_ascii_hexdigit())
        })
        && matches!(value.as_bytes()[14], b'1'..=b'8')
        && matches!(
            value.as_bytes()[19],
            b'8' | b'9' | b'a' | b'A' | b'b' | b'B'
        )
}
impl RunUsageRecord {
    pub fn parse(bytes: &[u8]) -> Result<Self, RunUsageError> {
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(RunUsageError::InvalidRecord);
        }
        let record: Self =
            serde_json::from_slice(bytes).map_err(|_| RunUsageError::InvalidRecord)?;
        record.validate()?;
        Ok(record)
    }
    pub fn validate(&self) -> Result<(), RunUsageError> {
        let valid = self.schema == EVENT_SCHEMA
            && self.event_id == self.run_id
            && valid_uuid(&self.run_id)
            && [&self.host_id, &self.account_id, &self.provider]
                .into_iter()
                .all(|value| valid_id(value))
            && !self.subject.is_empty()
            && self.subject.encode_utf16().count() <= 128
            && !self.vendor.is_empty()
            && self.vendor.encode_utf16().count() <= 80
            && !self.model.is_empty()
            && self.model.encode_utf16().count() <= 200
            && matches!(self.auth_mode.as_str(), "api-key" | "cli-session" | "none")
            && matches!(self.status.as_str(), "completed" | "failed" | "cancelled")
            && matches!(
                self.source.as_str(),
                "provider-response" | "cli-report" | "adapter-report" | "synthetic"
            )
            && self.started_at <= self.finished_at
            && (0..=9999).contains(&self.started_at.year())
            && (0..=9999).contains(&self.finished_at.year())
            && self.duration_ms <= MAX_SAFE_INTEGER
            && self
                .finished_at
                .signed_duration_since(self.started_at)
                .num_milliseconds()
                == self.duration_ms as i64
            && self.expires_at.is_none_or(|time| time > self.finished_at)
            && self.usage.valid()
            && self.observed_usage.valid()
            && self.usage.valid_subsets()
            && self.coverage.completed_steps <= self.coverage.started_steps
            && self.coverage.started_steps <= MAX_SAFE_INTEGER
            && self.metadata.len() <= 32
            && self.metadata.iter().all(|(key, value)| {
                key.encode_utf16().count() <= 64 && value.encode_utf16().count() <= 128
            });
        if !valid {
            return Err(RunUsageError::InvalidRecord);
        }
        for ((total, observed), reports) in self
            .usage
            .values()
            .into_iter()
            .zip(self.observed_usage.values())
            .zip(self.coverage.reported_steps.values())
        {
            let reports = reports.unwrap_or(0);
            if reports > self.coverage.completed_steps
                || (observed.is_some() && reports == 0)
                || (total.is_some()
                    && (total != observed
                        || self.coverage.started_steps == 0
                        || reports != self.coverage.started_steps
                        || self.coverage.completed_steps != self.coverage.started_steps))
            {
                return Err(RunUsageError::InvalidRecord);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StorePolicy {
    pub retention_days: u32,
    pub max_records: u64,
    pub max_bytes: u64,
}
impl Default for StorePolicy {
    fn default() -> Self {
        Self {
            retention_days: 30,
            max_records: 100_000,
            max_bytes: 256_000_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub schema: String,
    pub host_id: String,
    pub event_id: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRun {
    pub record: RunUsageRecord,
    pub expires_at: DateTime<Utc>,
    pub delivery: String,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_error: Option<String>,
}
pub struct RunUsageStore {
    connection: Connection,
    policy: StorePolicy,
}
type StoredRow = (Vec<u8>, i64, String, u32, Option<String>);

impl RunUsageStore {
    pub fn open(path: &Path, policy: StorePolicy) -> Result<Self, RunUsageError> {
        prepare_file(path)?;
        Self::from_connection(Connection::open(path)?, policy)
    }
    pub fn memory(policy: StorePolicy) -> Result<Self, RunUsageError> {
        Self::from_connection(Connection::open_in_memory()?, policy)
    }
    fn from_connection(connection: Connection, policy: StorePolicy) -> Result<Self, RunUsageError> {
        if !(1..=3650).contains(&policy.retention_days)
            || policy.max_records == 0
            || policy.max_records > 1_000_000
            || policy.max_bytes == 0
            || policy.max_bytes > 10_000_000_000
        {
            return Err(RunUsageError::InvalidPolicy);
        }
        connection.busy_timeout(Duration::from_secs(1))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;",
        )?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != 0 && version != 1 {
            return Err(RunUsageError::Storage);
        }
        connection.execute_batch("BEGIN IMMEDIATE;
          CREATE TABLE IF NOT EXISTS run_usage (host_id TEXT NOT NULL, event_id TEXT NOT NULL, payload BLOB NOT NULL, digest BLOB NOT NULL, expires_at INTEGER NOT NULL,
            delivery TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, retry_at INTEGER NOT NULL DEFAULT 0, delivery_error TEXT,
            PRIMARY KEY(host_id,event_id));
          CREATE INDEX IF NOT EXISTS run_usage_expiry ON run_usage(expires_at);
          CREATE TABLE IF NOT EXISTS run_usage_config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
          PRAGMA user_version=1; COMMIT;")?;
        Ok(Self { connection, policy })
    }
    /// A changed destination cannot silently redirect records accepted for another backend.
    pub fn bind_forward_target(&mut self, target: &str) -> Result<(), RunUsageError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = transaction
            .query_row(
                "SELECT value FROM run_usage_config WHERE key='forward_target'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if old.as_deref().is_some_and(|old| old != target) {
            let pending: u64 = transaction.query_row(
                "SELECT COUNT(*) FROM run_usage WHERE delivery IN ('pending','failed')",
                [],
                |row| row.get(0),
            )?;
            if pending > 0 {
                return Err(RunUsageError::TargetConflict);
            }
        }
        transaction.execute("INSERT INTO run_usage_config(key,value) VALUES('forward_target',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [target])?;
        transaction.commit()?;
        Ok(())
    }
    pub fn accept(
        &mut self,
        record: &RunUsageRecord,
        now: DateTime<Utc>,
        forward: bool,
    ) -> Result<Receipt, RunUsageError> {
        record.validate()?;
        if record.finished_at > now + TimeDelta::minutes(5) {
            return Err(RunUsageError::InvalidRecord);
        }
        let policy_expiry =
            record.finished_at + TimeDelta::days(i64::from(self.policy.retention_days));
        let expires = record
            .expires_at
            .map_or(policy_expiry, |value| value.min(policy_expiry));
        if expires <= now {
            return Err(RunUsageError::Expired);
        }
        let payload = serde_json::to_vec(record).map_err(|_| RunUsageError::InvalidRecord)?;
        if payload.len() > MAX_EVENT_BYTES {
            return Err(RunUsageError::InvalidRecord);
        }
        let digest = Sha256::digest(&payload).to_vec();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM run_usage WHERE expires_at<=?1",
            [now.timestamp_millis()],
        )?;
        let old: Option<(Vec<u8>, i64)> = transaction
            .query_row(
                "SELECT digest,expires_at FROM run_usage WHERE host_id=?1 AND event_id=?2",
                params![record.host_id, record.event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (status, expiry) = if let Some((old, expiry)) = old {
            if old != digest {
                return Err(RunUsageError::Conflict);
            }
            (
                "duplicate",
                DateTime::from_timestamp_millis(expiry).ok_or(RunUsageError::Storage)?,
            )
        } else {
            let (count, bytes): (u64, u64) = transaction.query_row(
                "SELECT COUNT(*),COALESCE(SUM(length(payload)),0) FROM run_usage",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if count >= self.policy.max_records
                || bytes + payload.len() as u64 > self.policy.max_bytes
            {
                return Err(RunUsageError::Capacity);
            }
            transaction.execute("INSERT INTO run_usage(host_id,event_id,payload,digest,expires_at,delivery) VALUES(?1,?2,?3,?4,?5,?6)", params![record.host_id, record.event_id, payload, digest, expires.timestamp_millis(), if forward { "pending" } else { "local" }])?;
            ("accepted", expires)
        };
        transaction.commit()?;
        Ok(Receipt {
            schema: "usagestat.run-receipt.v1".into(),
            host_id: record.host_id.clone(),
            event_id: record.event_id.clone(),
            status: status.into(),
            expires_at: expiry,
        })
    }
    pub fn purge(&mut self, now: DateTime<Utc>) -> Result<usize, RunUsageError> {
        let removed = self.connection.execute(
            "DELETE FROM run_usage WHERE expires_at<=?1",
            [now.timestamp_millis()],
        )?;
        if removed > 0 {
            self.connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        }
        Ok(removed)
    }
    pub fn get(
        &mut self,
        host: &str,
        event: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<StoredRun>, RunUsageError> {
        self.purge(now)?;
        let value: Option<StoredRow> = self.connection.query_row("SELECT payload,expires_at,delivery,attempts,delivery_error FROM run_usage WHERE host_id=?1 AND event_id=?2", params![host, event], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
        value
            .map(|(payload, expires, delivery, attempts, error)| {
                Ok(StoredRun {
                    record: RunUsageRecord::parse(&payload).map_err(|_| RunUsageError::Storage)?,
                    expires_at: DateTime::from_timestamp_millis(expires)
                        .ok_or(RunUsageError::Storage)?,
                    delivery,
                    attempts,
                    delivery_error: error,
                })
            })
            .transpose()
    }
    pub fn next_pending(&mut self, now: DateTime<Utc>) -> Result<Option<StoredRun>, RunUsageError> {
        self.purge(now)?;
        let key: Option<(String, String)> = self.connection.query_row("SELECT host_id,event_id FROM run_usage WHERE delivery='pending' AND retry_at<=?1 ORDER BY retry_at,event_id LIMIT 1", [now.timestamp_millis()], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        match key {
            Some((host, event)) => self.get(&host, &event, now),
            None => Ok(None),
        }
    }
    pub fn delivery_result(
        &mut self,
        host: &str,
        event: &str,
        state: &str,
        code: Option<&str>,
        retry_at: DateTime<Utc>,
    ) -> Result<(), RunUsageError> {
        if !matches!(state, "pending" | "delivered" | "failed")
            || code.is_some_and(|code| {
                !code.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') || code.len() > 64
            })
        {
            return Err(RunUsageError::InvalidPolicy);
        }
        self.connection.execute("UPDATE run_usage SET delivery=?3,delivery_error=?4,retry_at=?5,attempts=MIN(attempts+1,4294967295) WHERE host_id=?1 AND event_id=?2 AND delivery='pending'", params![host,event,state,code,retry_at.timestamp_millis()])?;
        Ok(())
    }
    /// Explicit operator retry after repairing a permanent forwarding failure.
    /// Never changes the event payload, its original expiry or an acknowledged delivery.
    pub fn retry_failed(
        &mut self,
        host: &str,
        event: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, RunUsageError> {
        self.purge(now)?;
        Ok(self.connection.execute("UPDATE run_usage SET delivery='pending',delivery_error=NULL,retry_at=0 WHERE host_id=?1 AND event_id=?2 AND delivery='failed'",params![host,event])? > 0)
    }
}

fn prepare_file(path: &Path) -> Result<(), RunUsageError> {
    if !path.is_absolute() {
        return Err(RunUsageError::Permissions);
    }
    let parent = path.parent().ok_or(RunUsageError::Permissions)?;
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory
        .create(parent)
        .map_err(|_| RunUsageError::Storage)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| RunUsageError::Storage)?;
    if !metadata.is_dir() {
        return Err(RunUsageError::Permissions);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(RunUsageError::Permissions);
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err(RunUsageError::Storage),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| RunUsageError::Storage)?;
    if !metadata.is_file() {
        return Err(RunUsageError::Permissions);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(RunUsageError::Permissions);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture() -> RunUsageRecord {
        RunUsageRecord::parse(br#"{
          "schema":"agenticdriver.usage.v2",
          "eventId":"12345678-1234-4234-8234-123456789abc",
          "runId":"12345678-1234-4234-8234-123456789abc",
          "hostId":"host-one","accountId":"account-one","subject":"app-one",
          "provider":"openai-work","vendor":"openai","model":"explicit-model",
          "authMode":"api-key","status":"failed","source":"provider-response",
          "startedAt":"2026-09-21T00:00:00.000Z","finishedAt":"2026-09-21T00:00:01.000Z",
          "durationMs":1000,"usage":{},"observedUsage":{"inputTokens":12,"outputTokens":0},
          "coverage":{"startedSteps":2,"completedSteps":1,"reportedSteps":{"inputTokens":1,"outputTokens":1}},
          "metadata":{"application":"fixture"}
        }"#).unwrap()
    }
    fn now() -> DateTime<Utc> {
        fixture().finished_at
    }
    fn second(mut record: RunUsageRecord) -> RunUsageRecord {
        record.event_id = "87654321-1234-4234-8234-123456789abc".into();
        record.run_id = record.event_id.clone();
        record
    }

    #[test]
    fn rejects_ambiguous_private_and_inconsistent_records() {
        let base = serde_json::to_value(fixture()).unwrap();
        for (pointer, invalid) in [
            ("/schema", json!("agenticdriver.usage.v3")),
            ("/eventId", json!("87654321-1234-4234-8234-123456789abc")),
            ("/accountId", Value::Null),
            ("/hostId", json!("../outside")),
            ("/usage", json!({"inputTokens":null})),
            ("/usage", json!({"inputTokens":1.5})),
            ("/usage", json!({"inputTokens":-1})),
            ("/usage", json!({"inputTokens":9007199254740992u64})),
            ("/usage", json!({"inputTokens":12})),
            ("/observedUsage", json!({"cachedInputTokens":1})),
            ("/coverage/completedSteps", json!(3)),
            ("/coverage/reportedSteps/inputTokens", json!(2)),
            ("/durationMs", json!(1001)),
        ] {
            let mut value = base.clone();
            *value.pointer_mut(pointer).unwrap() = invalid;
            assert!(
                RunUsageRecord::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
                "{pointer}: {value}"
            );
        }
        let mut value = base;
        value["prompt"] = json!("private context");
        assert!(RunUsageRecord::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(RunUsageRecord::parse(&vec![b' '; MAX_EVENT_BYTES + 1]).is_err());
    }

    #[test]
    fn preserves_partial_measurements_deduplicates_and_scopes_identity() {
        let mut store = RunUsageStore::memory(StorePolicy::default()).unwrap();
        let record = fixture();
        assert_eq!(
            store.accept(&record, now(), true).unwrap().status,
            "accepted"
        );
        assert_eq!(
            store.accept(&record, now(), true).unwrap().status,
            "duplicate"
        );
        let found = store
            .get(&record.host_id, &record.event_id, now())
            .unwrap()
            .unwrap();
        assert_eq!(found.record, record);
        assert_eq!(found.record.usage.input_tokens, None);
        assert_eq!(found.record.observed_usage.output_tokens, Some(0));
        assert_eq!(found.delivery, "pending");
        assert_eq!(found.attempts, 0);
        let mut changed = record.clone();
        changed.subject = "another-app".into();
        assert!(matches!(
            store.accept(&changed, now(), true),
            Err(RunUsageError::Conflict)
        ));
        changed.host_id = "another-host".into();
        assert_eq!(
            store.accept(&changed, now(), true).unwrap().status,
            "accepted"
        );
    }

    #[test]
    fn json_integer_notation_and_unicode_follow_the_sdk_schema() {
        let record = fixture();
        let serialized = serde_json::to_string(&record)
            .unwrap()
            .replace("\"inputTokens\":12", "\"inputTokens\":1.2e1")
            .replace("\"durationMs\":1000", "\"durationMs\":1000.0");
        assert_eq!(
            RunUsageRecord::parse(serialized.as_bytes()).unwrap(),
            record
        );
        let mut invalid = record.clone();
        invalid.subject = "😀".repeat(65);
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn retention_caps_purges_and_rejects_expired_replay() {
        let policy = StorePolicy {
            retention_days: 1,
            ..StorePolicy::default()
        };
        let mut store = RunUsageStore::memory(policy).unwrap();
        let mut record = fixture();
        record.expires_at = Some(now() + TimeDelta::days(10));
        let receipt = store.accept(&record, now(), false).unwrap();
        assert_eq!(receipt.expires_at, now() + TimeDelta::days(1));
        assert!(
            store
                .get(&record.host_id, &record.event_id, receipt.expires_at)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            store.accept(&record, receipt.expires_at, false),
            Err(RunUsageError::Expired)
        ));
        let mut short = second(fixture());
        short.expires_at = Some(now() + TimeDelta::hours(1));
        assert_eq!(
            store.accept(&short, now(), false).unwrap().expires_at,
            short.expires_at.unwrap()
        );
    }

    #[test]
    fn capacity_rejects_new_records_without_losing_pending_work() {
        let mut store = RunUsageStore::memory(StorePolicy {
            max_records: 1,
            ..StorePolicy::default()
        })
        .unwrap();
        let record = fixture();
        store.accept(&record, now(), true).unwrap();
        assert!(matches!(
            store.accept(&second(record.clone()), now(), true),
            Err(RunUsageError::Capacity)
        ));
        assert_eq!(
            store.accept(&record, now(), true).unwrap().status,
            "duplicate"
        );
        assert_eq!(store.next_pending(now()).unwrap().unwrap().record, record);
        let mut tiny = RunUsageStore::memory(StorePolicy {
            max_bytes: 1,
            ..StorePolicy::default()
        })
        .unwrap();
        assert!(matches!(
            tiny.accept(&record, now(), false),
            Err(RunUsageError::Capacity)
        ));
    }

    #[test]
    fn forwarding_retries_acknowledgements_and_destination_changes_are_durable() {
        let mut store = RunUsageStore::memory(StorePolicy::default()).unwrap();
        let record = fixture();
        store.bind_forward_target("https://one.example").unwrap();
        store.accept(&record, now(), true).unwrap();
        assert!(matches!(
            store.bind_forward_target("https://two.example"),
            Err(RunUsageError::TargetConflict)
        ));
        let retry = now() + TimeDelta::seconds(10);
        store
            .delivery_result(
                &record.host_id,
                &record.event_id,
                "pending",
                Some("NETWORK"),
                retry,
            )
            .unwrap();
        assert!(store.next_pending(now()).unwrap().is_none());
        assert_eq!(store.next_pending(retry).unwrap().unwrap().attempts, 1);
        store
            .delivery_result(&record.host_id, &record.event_id, "delivered", None, retry)
            .unwrap();
        assert!(store.next_pending(retry).unwrap().is_none());
        // Retrying a source POST after its acknowledgement was lost never re-enqueues delivery.
        assert_eq!(
            store.accept(&record, retry, true).unwrap().status,
            "duplicate"
        );
        assert_eq!(
            store
                .get(&record.host_id, &record.event_id, retry)
                .unwrap()
                .unwrap()
                .delivery,
            "delivered"
        );
        store.bind_forward_target("https://two.example").unwrap();
    }

    #[test]
    fn committed_records_and_pending_delivery_survive_reopen() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "usagestat-runs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("private/events.sqlite");
        let record = fixture();
        {
            let mut store = RunUsageStore::open(&path, StorePolicy::default()).unwrap();
            store.accept(&record, now(), true).unwrap();
        }
        {
            let mut store = RunUsageStore::open(&path, StorePolicy::default()).unwrap();
            assert_eq!(store.next_pending(now()).unwrap().unwrap().record, record);
            assert_eq!(
                store.accept(&record, now(), true).unwrap().status,
                "duplicate"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(matches!(
                RunUsageStore::open(&path, StorePolicy::default()),
                Err(RunUsageError::Permissions)
            ));
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
