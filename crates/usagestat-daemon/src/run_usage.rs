//! Optional native execution ingestion and durable forwarding. No provider probes.
use crate::{cliproxy::keys_equal, http_request::Request};
use anyhow::{Result, bail};
use chrono::{TimeDelta, Utc};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use usagestat_core::run_usage::{
    EVENT_SCHEMA, MAX_EVENT_BYTES, Receipt, RunUsageError, RunUsageRecord, RunUsageStore,
    StorePolicy, valid_id,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    version: u32,
    database: PathBuf,
    clients: Vec<ClientConfig>,
    #[serde(default = "retention")]
    retention_days: u32,
    #[serde(default = "records")]
    max_records: u64,
    #[serde(default = "bytes")]
    max_bytes: u64,
    forward: Option<ForwardConfig>,
}
fn retention() -> u32 {
    30
}
fn records() -> u64 {
    100_000
}
fn bytes() -> u64 {
    256_000_000
}
fn timeout() -> u64 {
    2_000
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientConfig {
    token: SecretRef,
    bindings: Vec<Binding>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    host_id: String,
    provider: String,
    account_id: String,
    subjects: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForwardConfig {
    url: String,
    token: SecretRef,
    #[serde(default = "timeout")]
    timeout_ms: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SecretRef {
    Env(String),
    File(PathBuf),
}
struct SourceClient {
    token: String,
    bindings: Vec<Binding>,
}
struct Forward {
    url: Url,
    token: String,
    client: Client,
}
pub struct RunUsageApi {
    store: Mutex<RunUsageStore>,
    clients: Vec<SourceClient>,
    forward: Option<Forward>,
}

fn private_bytes(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| anyhow::anyhow!("run usage configuration or credential is unavailable"))?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        bail!("run usage configuration or credential must be a bounded regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("run usage configuration and credential files must be private (mode 0600)");
        }
    }
    let mut output = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut output)?;
    if output.len() > limit {
        bail!("run usage file exceeds its size limit");
    }
    Ok(output)
}
fn resolve(secret: SecretRef, base: &Path) -> Result<String> {
    let token = match secret {
        SecretRef::Env(name) => {
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                bail!("invalid run usage credential reference");
            }
            std::env::var(name)
                .map_err(|_| anyhow::anyhow!("run usage credential is unavailable"))?
        }
        SecretRef::File(path) => String::from_utf8(private_bytes(&base.join(path), 4096)?)
            .map_err(|_| anyhow::anyhow!("run usage credential must be UTF-8"))?
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
    };
    if !(32..=4096).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        bail!("run usage credentials require 32 to 4096 non-whitespace ASCII characters");
    }
    Ok(token)
}
fn target_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("invalid run usage backend URL"))?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        bail!(
            "run usage forwarding requires an HTTPS origin or loopback HTTP origin without embedded credentials"
        );
    }
    Ok(url)
}

impl SourceClient {
    fn permits(&self, record: &RunUsageRecord) -> bool {
        self.bindings.iter().any(|b| {
            b.host_id == record.host_id
                && b.provider == record.provider
                && b.account_id == record.account_id
                && b.subjects.contains(&record.subject)
        })
    }
}
impl RunUsageApi {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Config = serde_json::from_slice(&private_bytes(path, 65536)?)
            .map_err(|_| anyhow::anyhow!("invalid run usage configuration"))?;
        if config.version != 1 || config.clients.is_empty() || config.clients.len() > 128 {
            bail!("invalid run usage configuration version or clients");
        }
        let base = path.parent().unwrap_or(Path::new(".")).canonicalize()?;
        let mut clients: Vec<SourceClient> = Vec::new();
        for entry in config.clients {
            let token = resolve(entry.token, &base)?;
            if clients.iter().any(|other| keys_equal(&other.token, &token)) {
                bail!("duplicate run usage client credential");
            }
            if entry.bindings.is_empty()
                || entry.bindings.len() > 128
                || entry.bindings.iter().any(|b| {
                    !valid_id(&b.host_id)
                        || !valid_id(&b.provider)
                        || !valid_id(&b.account_id)
                        || b.subjects.is_empty()
                        || b.subjects.len() > 128
                        || b.subjects
                            .iter()
                            .any(|s| s.is_empty() || s.chars().count() > 128)
                })
            {
                bail!("invalid run usage account bindings");
            }
            clients.push(SourceClient {
                token,
                bindings: entry.bindings,
            });
        }
        let mut store = RunUsageStore::open(
            &base.join(config.database),
            StorePolicy {
                retention_days: config.retention_days,
                max_records: config.max_records,
                max_bytes: config.max_bytes,
            },
        )?;
        store.purge(Utc::now())?;
        let forward = if let Some(config) = config.forward {
            if !(100..=30_000).contains(&config.timeout_ms) {
                bail!("run usage forwarding timeout must be between 100 and 30000 milliseconds");
            }
            let url = target_url(&config.url)?;
            let token = resolve(config.token, &base)?;
            let _ = rustls::crypto::ring::default_provider().install_default();
            let client = Client::builder()
                .redirect(Policy::none())
                .timeout(Duration::from_millis(config.timeout_ms))
                .no_proxy()
                .build()
                .map_err(|_| anyhow::anyhow!("run usage forwarding client is unavailable"))?;
            store.bind_forward_target(url.as_str())?;
            Some(Forward { url, token, client })
        } else {
            None
        };
        Ok(Self {
            store: Mutex::new(store),
            clients,
            forward,
        })
    }
    pub fn handles(path: &str) -> bool {
        path == "/v1/run-usage" || path.starts_with("/v1/run-usage/")
    }
    pub fn route(&self, request: &Request) -> String {
        // No browser-readable metering endpoint or inherited wildcard CORS policy.
        if request.headers.iter().any(|(key, _)| key == "origin") {
            return error(403, "ORIGIN_FORBIDDEN");
        }
        let supplied = request
            .header("authorization")
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, key)| key);
        let source = supplied.and_then(|key| {
            self.clients
                .iter()
                .find(|source| keys_equal(&source.token, key))
        });
        let Some(source) = source else {
            return error(401, "UNAUTHORIZED");
        };
        if request.method == "GET" && request.path == "/v1/run-usage/protocol" {
            return response(
                200,
                json!({"schema":"usagestat.run-ingestion.v1","eventSchemas":[EVENT_SCHEMA],"receiptSchema":"usagestat.run-receipt.v1","maxEventBytes":MAX_EVENT_BYTES,"requiresAccount":true}),
            );
        }
        if request.method == "POST" && request.path == "/v1/run-usage" {
            if request.header("content-type").is_none_or(|v| {
                !v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/json")
            }) {
                return error(415, "CONTENT_TYPE");
            }
            let record = match RunUsageRecord::parse(&request.body) {
                Ok(v) => v,
                Err(_) => return error(400, "INVALID_RECORD"),
            };
            if !source.permits(&record) {
                return error(403, "ACCOUNT_FORBIDDEN");
            }
            let Ok(mut store) = self.store.lock() else {
                return error(503, "STORAGE_UNAVAILABLE");
            };
            return match store.accept(&record, Utc::now(), self.forward.is_some()) {
                Ok(receipt) => response(
                    if receipt.status == "accepted" {
                        201
                    } else {
                        200
                    },
                    json!(receipt),
                ),
                Err(RunUsageError::Conflict) => error(409, "EVENT_CONFLICT"),
                Err(RunUsageError::Expired) => error(410, "EVENT_EXPIRED"),
                Err(RunUsageError::Capacity) => error(429, "STORE_CAPACITY"),
                Err(RunUsageError::InvalidRecord) => error(400, "INVALID_RECORD"),
                Err(_) => error(503, "STORAGE_UNAVAILABLE"),
            };
        }
        if request.method == "POST" && request.path.ends_with("/retry") {
            if self.forward.is_none() {
                return error(409, "FORWARDING_DISABLED");
            }
            let tail = request
                .path
                .strip_prefix("/v1/run-usage/")
                .and_then(|v| v.strip_suffix("/retry"));
            if let Some((host, event)) = tail.and_then(|v| v.split_once('/')) {
                if !source.bindings.iter().any(|b| b.host_id == host) {
                    return error(404, "NOT_FOUND");
                }
                let Ok(mut store) = self.store.lock() else {
                    return error(503, "STORAGE_UNAVAILABLE");
                };
                match store.get(host, event, Utc::now()) {
                    Ok(Some(stored)) if source.permits(&stored.record) => {
                        match store.retry_failed(host, event, Utc::now()) {
                            Ok(true) => {
                                return response(
                                    200,
                                    json!({"schema":"usagestat.run-retry.v1","hostId":host,"eventId":event,"status":"queued"}),
                                );
                            }
                            Ok(false) => return error(409, "DELIVERY_NOT_FAILED"),
                            Err(_) => return error(503, "STORAGE_UNAVAILABLE"),
                        }
                    }
                    Ok(_) => return error(404, "NOT_FOUND"),
                    Err(_) => return error(503, "STORAGE_UNAVAILABLE"),
                }
            }
            return error(404, "NOT_FOUND");
        }
        if request.method == "GET" {
            if let Some(tail) = request.path.strip_prefix("/v1/run-usage/")
                && let Some((host, event)) = tail.split_once('/')
            {
                if !source.bindings.iter().any(|b| b.host_id == host) {
                    return error(404, "NOT_FOUND");
                }
                let Ok(mut store) = self.store.lock() else {
                    return error(503, "STORAGE_UNAVAILABLE");
                };
                return match store.get(host, event, Utc::now()) {
                    Ok(Some(stored)) if source.permits(&stored.record) => response(
                        200,
                        json!({"schema":"usagestat.stored-run.v1","record":stored.record,"expiresAt":stored.expires_at,"delivery":stored.delivery,"attempts":stored.attempts,"deliveryError":stored.delivery_error}),
                    ),
                    Ok(_) => error(404, "NOT_FOUND"),
                    Err(_) => error(503, "STORAGE_UNAVAILABLE"),
                };
            }
            return error(404, "NOT_FOUND");
        }
        error(405, "METHOD_NOT_ALLOWED")
    }
    pub fn start(self: &Arc<Self>, shutdown: Arc<AtomicBool>) -> thread::JoinHandle<()> {
        let api = Arc::clone(self);
        thread::spawn(move || {
            while !shutdown.load(Ordering::SeqCst) {
                // Housekeeping cadence and transport timeout do not limit agent execution.
                if api.forward.is_some() {
                    api.forward_once();
                } else if let Ok(mut store) = api.store.lock() {
                    let _ = store.purge(Utc::now());
                }
                let deadline = Instant::now()
                    + Duration::from_millis(if api.forward.is_some() { 250 } else { 30_000 });
                while !shutdown.load(Ordering::SeqCst) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(25));
                }
            }
        })
    }
    fn forward_once(&self) {
        let Some(target) = &self.forward else { return };
        let pending = self
            .store
            .lock()
            .ok()
            .and_then(|mut store| store.next_pending(Utc::now()).ok().flatten());
        let Some(pending) = pending else { return };
        let result = deliver(target, &pending.record);
        let (state, code) = match result {
            Ok(()) => ("delivered", None),
            Err((true, code)) => ("pending", Some(code)),
            Err((false, code)) => ("failed", Some(code)),
        };
        let backoff = 1i64 << pending.attempts.min(6);
        if let Ok(mut store) = self.store.lock() {
            // Commit before acknowledge at both ends. A crash here safely resends the same identity.
            if store
                .delivery_result(
                    &pending.record.host_id,
                    &pending.record.event_id,
                    state,
                    code,
                    Utc::now() + TimeDelta::seconds(backoff.min(60)),
                )
                .is_err()
            {
                log::warn!("run usage delivery state could not be persisted");
            }
        }
    }
}
fn deliver(
    target: &Forward,
    record: &RunUsageRecord,
) -> std::result::Result<(), (bool, &'static str)> {
    let url = target
        .url
        .join("v1/run-usage")
        .map_err(|_| (false, "CONFIGURATION"))?;
    let response = target
        .client
        .post(url)
        .bearer_auth(&target.token)
        .json(record)
        .send()
        .map_err(|_| (true, "NETWORK"))?;
    let status = response.status().as_u16();
    if !matches!(status, 200 | 201) {
        return Err((
            matches!(status, 408 | 429 | 500..=599),
            match status {
                401 | 403 => "AUTHORIZATION",
                409 => "EVENT_CONFLICT",
                410 => "EVENT_EXPIRED",
                429 => "STORE_CAPACITY",
                500..=599 => "BACKEND_UNAVAILABLE",
                _ => "PROTOCOL",
            },
        ));
    }
    let mut bytes = Vec::new();
    response
        .take(16_385)
        .read_to_end(&mut bytes)
        .map_err(|_| (true, "NETWORK"))?;
    if bytes.len() > 16_384 {
        return Err((false, "PROTOCOL"));
    }
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| (false, "PROTOCOL"))?;
    if receipt.schema != "usagestat.run-receipt.v1"
        || receipt.host_id != record.host_id
        || receipt.event_id != record.event_id
        || !matches!(receipt.status.as_str(), "accepted" | "duplicate")
        || receipt.expires_at <= Utc::now()
    {
        return Err((false, "PROTOCOL"));
    }
    Ok(())
}
fn response(status: u16, body: serde_json::Value) -> String {
    let body = body.to_string();
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        410 => "Gone",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        _ => "Service Unavailable",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
pub fn error(status: u16, code: &str) -> String {
    response(
        status,
        json!({"schema":"usagestat.run-error.v1","error":code}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwarding_requires_secure_explicit_origins() {
        for value in [
            "http://example.com",
            "https://user:secret@example.com",
            "https://example.com?token=secret",
            "https://example.com/path",
            "file:///tmp/backend",
            "https://example.com/#fragment",
            "http://localhost",
        ] {
            assert!(target_url(value).is_err(), "{value}");
        }
        for value in [
            "https://backend.example",
            "http://127.0.0.1:1234",
            "http://[::1]:1234",
        ] {
            assert!(target_url(value).is_ok(), "{value}");
        }
    }
}
