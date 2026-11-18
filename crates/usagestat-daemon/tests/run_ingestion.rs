//! Real native daemons, private fixture credentials, and no provider polling.
use chrono::{SecondsFormat, TimeDelta, Utc};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const TOKEN: &str = "fixture-ingestion-token-at-least-32-characters";
const OTHER: &str = "fixture-other-account-token-at-least-32-characters";
const EVENT: &str = "12345678-1234-4234-8234-123456789abc";
const LOOKUP: &str = "/v1/run-usage/host-one/12345678-1234-4234-8234-123456789abc";
fn client() -> Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}
fn private(path: &std::path::Path, bytes: impl AsRef<[u8]>) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
fn record() -> Value {
    let end = Utc::now();
    let start = end - TimeDelta::seconds(1);
    json!({"schema":"agenticdriver.usage.v2","eventId":EVENT,"runId":EVENT,"hostId":"host-one","accountId":"account-one","subject":"app-one","provider":"configured-provider","vendor":"fixture","model":"fixture-model","authMode":"none","status":"failed","source":"synthetic","startedAt":start.to_rfc3339_opts(SecondsFormat::Millis,true),"finishedAt":end.to_rfc3339_opts(SecondsFormat::Millis,true),"durationMs":1000,"usage":{},"observedUsage":{"inputTokens":5,"outputTokens":0},"coverage":{"startedSteps":2,"completedSteps":1,"reportedSteps":{"inputTokens":1,"outputTokens":1}},"metadata":{}})
}
struct Daemon {
    directory: PathBuf,
    url: String,
    child: Option<Child>,
}
impl Daemon {
    fn new(forward: Option<&str>, max_records: u32) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "usagestat-ingestion-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut config = json!({"version":1,"database":"runs/events.sqlite","retentionDays":1,"maxRecords":max_records,"clients":[
            {"token":{"file":"ingest.key"},"bindings":[{"hostId":"host-one","provider":"configured-provider","accountId":"account-one","subjects":["app-one"]}]},
            {"token":{"file":"other.key"},"bindings":[{"hostId":"host-one","provider":"configured-provider","accountId":"account-other","subjects":["app-other"]}]}
        ]});
        if let Some(url) = forward {
            config["forward"] = json!({"url":url,"token":{"file":"ingest.key"},"timeoutMs":500});
        }
        private(&directory.join("ingest.key"), TOKEN);
        private(&directory.join("other.key"), OTHER);
        private(
            &directory.join("run-usage.json"),
            serde_json::to_vec(&config).unwrap(),
        );
        Self {
            directory,
            url: format!("http://127.0.0.1:{port}"),
            child: None,
        }
    }
    fn start(&mut self) {
        assert!(self.child.is_none());
        let mut command = Command::new(env!("CARGO_BIN_EXE_usagestatd"));
        command.env_clear();
        // Windows networking needs the system directory even in an otherwise
        // isolated child environment. Do not inherit provider credentials.
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        let log = self.directory.join("daemon.log");
        self.child = Some(
            command
                .env("USAGESTAT_CONFIG_DIR", self.directory.join("config"))
                .env("USAGESTAT_DATA_DIR", self.directory.join("data"))
                .args([
                    "--no-poll",
                    "--bind",
                    self.url.strip_prefix("http://").unwrap(),
                    "--run-usage-config",
                ])
                .arg(self.directory.join("run-usage.json"))
                .stdout(Stdio::null())
                .stderr(fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let exited = self.child.as_mut().unwrap().try_wait().unwrap();
            assert!(
                exited.is_none() && Instant::now() < deadline,
                "daemon failed to become ready (exit: {exited:?}): {}",
                fs::read_to_string(&log).unwrap_or_default()
            );
            if client().get(format!("{}/health", self.url)).send().is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn post(&self, value: &Value) -> reqwest::blocking::Response {
        client()
            .post(format!("{}/v1/run-usage", self.url))
            .bearer_auth(TOKEN)
            .json(value)
            .send()
            .unwrap()
    }
    fn get(&self, token: &str) -> reqwest::blocking::Response {
        client()
            .get(format!("{}{LOOKUP}", self.url))
            .bearer_auth(token)
            .send()
            .unwrap()
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(12);
    while !predicate() {
        assert!(Instant::now() < end, "fixture condition did not complete");
        thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn authenticated_native_capture_preserves_unknowns_and_enforces_scope_and_capacity() {
    let mut daemon = Daemon::new(None, 1);
    daemon.start();
    let report = record();
    assert_eq!(
        client()
            .post(format!("{}/v1/run-usage", daemon.url))
            .json(&report)
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client()
            .get(format!("{}/v1/run-usage/protocol", daemon.url))
            .bearer_auth(TOKEN)
            .send()
            .unwrap()
            .json::<Value>()
            .unwrap()["requiresAccount"],
        true
    );
    let accepted = daemon.post(&report);
    assert_eq!(accepted.status(), 201);
    assert!(
        accepted
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    assert_eq!(
        daemon.post(&report).json::<Value>().unwrap()["status"],
        "duplicate"
    );
    let found = daemon.get(TOKEN).json::<Value>().unwrap();
    assert_eq!(found["record"]["usage"], json!({}));
    assert_eq!(found["record"]["observedUsage"], report["observedUsage"]);
    assert_eq!(daemon.get(OTHER).status(), 404);
    let mut denied = report.clone();
    denied["accountId"] = json!("account-other");
    assert_eq!(daemon.post(&denied).status(), 403);
    denied = report.clone();
    denied["subject"] = json!("app-other");
    assert_eq!(daemon.post(&denied).status(), 403);
    let mut changed = report.clone();
    changed["model"] = json!("changed-model");
    assert_eq!(daemon.post(&changed).status(), 409);
    let mut second = report.clone();
    second["eventId"] = json!("87654321-1234-4234-8234-123456789abc");
    second["runId"] = second["eventId"].clone();
    assert_eq!(daemon.post(&second).status(), 429);
    let mut invalid = report.clone();
    invalid["usage"] = json!({"inputTokens":null});
    assert_eq!(daemon.post(&invalid).status(), 400);
    invalid = report.clone();
    invalid["prompt"] = json!("should never be stored");
    assert_eq!(daemon.post(&invalid).status(), 400);
    assert_eq!(
        client()
            .post(format!("{}/v1/run-usage", daemon.url))
            .bearer_auth(TOKEN)
            .header("Origin", "https://application.example")
            .json(&report)
            .send()
            .unwrap()
            .status(),
        403
    );
    // Existing polling reads still work and never acquire metering records.
    assert_eq!(
        client()
            .get(format!("{}/v1/usage", daemon.url))
            .send()
            .unwrap()
            .json::<Value>()
            .unwrap(),
        json!([])
    );
    daemon.stop();
    daemon.start();
    assert_eq!(daemon.post(&report).status(), 200);
    assert_eq!(
        daemon.get(TOKEN).json::<Value>().unwrap()["record"]["observedUsage"],
        report["observedUsage"]
    );
}

#[test]
fn native_forwarding_recovers_after_offline_period_and_source_restart() {
    let mut destination = Daemon::new(None, 10);
    let mut source = Daemon::new(Some(&destination.url), 1);
    source.start();
    let report = record();
    assert_eq!(source.post(&report).status(), 201);
    wait_for(|| {
        source.get(TOKEN).json::<Value>().unwrap()["attempts"]
            .as_u64()
            .unwrap()
            > 0
    });
    assert_eq!(
        source.get(TOKEN).json::<Value>().unwrap()["delivery"],
        "pending"
    );
    source.stop();
    destination.start();
    source.start();
    wait_for(|| source.get(TOKEN).json::<Value>().unwrap()["delivery"] == "delivered");
    assert_eq!(
        destination.get(TOKEN).json::<Value>().unwrap()["record"]["observedUsage"],
        report["observedUsage"]
    );
    assert_eq!(destination.post(&report).status(), 200);
    assert_eq!(source.post(&report).status(), 200);
}

#[test]
fn permanent_forwarding_failure_requires_an_authorized_explicit_retry() {
    let mut destination = Daemon::new(None, 10);
    private(
        &destination.directory.join("ingest.key"),
        "different-destination-token-at-least-32-characters",
    );
    destination.start();
    let mut source = Daemon::new(Some(&destination.url), 10);
    source.start();
    let report = record();
    assert_eq!(source.post(&report).status(), 201);
    wait_for(|| source.get(TOKEN).json::<Value>().unwrap()["delivery"] == "failed");
    assert_eq!(
        source.get(TOKEN).json::<Value>().unwrap()["deliveryError"],
        "AUTHORIZATION"
    );
    // Ordinary duplicate ingestion cannot silently restart a permanent failure.
    assert_eq!(source.post(&report).status(), 200);
    assert_eq!(
        source.get(TOKEN).json::<Value>().unwrap()["delivery"],
        "failed"
    );
    let retry_url = format!("{}{LOOKUP}/retry", source.url);
    assert_eq!(
        client()
            .post(&retry_url)
            .bearer_auth(OTHER)
            .send()
            .unwrap()
            .status(),
        404
    );
    destination.stop();
    private(&destination.directory.join("ingest.key"), TOKEN);
    destination.start();
    assert_eq!(
        client()
            .post(&retry_url)
            .bearer_auth(TOKEN)
            .send()
            .unwrap()
            .status(),
        200
    );
    wait_for(|| source.get(TOKEN).json::<Value>().unwrap()["delivery"] == "delivered");
    assert_eq!(
        client()
            .post(&retry_url)
            .bearer_auth(TOKEN)
            .send()
            .unwrap()
            .status(),
        409
    );
}

#[test]
fn lost_destination_acknowledgement_retries_the_same_event_once() {
    let mut destination = Daemon::new(None, 10);
    destination.start();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = stopped.clone();
    let target = destination.url.clone();
    let worker = thread::spawn(move || {
        let mut statuses = Vec::new();
        while !worker_stop.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            assert!(length < 65537);
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            drop(reader);
            let response = client()
                .post(format!("{target}/v1/run-usage"))
                .bearer_auth(TOKEN)
                .header("Content-Type", "application/json")
                .body(bytes)
                .send()
                .unwrap();
            let status = response.status().as_u16();
            let body = response.text().unwrap();
            statuses.push(status);
            if statuses.len() == 1 {
                continue;
            } // Backend committed; source receives no acknowledgement.
            write!(stream,"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        statuses
    });
    let mut source = Daemon::new(Some(&proxy_url), 10);
    source.start();
    assert_eq!(source.post(&record()).status(), 201);
    wait_for(|| source.get(TOKEN).json::<Value>().unwrap()["delivery"] == "delivered");
    stopped.store(true, Ordering::Relaxed);
    assert_eq!(worker.join().unwrap(), vec![201, 200]);
    assert_eq!(source.get(TOKEN).json::<Value>().unwrap()["attempts"], 2);
}
