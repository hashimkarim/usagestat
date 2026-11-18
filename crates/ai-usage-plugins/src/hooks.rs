//! Hook delivery: run a command hook and/or POST a webhook for one event.
//!
//! Delivery is synchronous and bounded by the hook's timeout; callers that
//! must not block (the daemon poller) run it on worker threads. Error
//! messages never include header values, URLs or command arguments, which
//! may carry tokens.

use std::time::Duration;
use usagestat_core::hooks::{HookConfig, HookPayload};

const OUTPUT_LIMIT: usize = 64 * 1024;

/// Deliver `payload` to every target of `hook`. All targets are attempted;
/// the error lists each failed target.
pub fn deliver(hook: &HookConfig, payload: &HookPayload) -> Result<(), String> {
    hook.validate()?;
    let json = serde_json::to_string(payload).map_err(|_| "payload could not be encoded")?;
    let mut failures = Vec::new();
    if !hook.command.is_empty()
        && let Err(error) = run_command(hook, payload, &json)
    {
        failures.push(format!(
            "command {}: {error}",
            hook.program_name().unwrap_or_default()
        ));
    }
    if let Some(url) = &hook.url
        && let Err(error) = post(hook, url, &json)
    {
        failures.push(format!("webhook: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn run_command(hook: &HookConfig, payload: &HookPayload, json: &str) -> Result<(), String> {
    let (program, arguments) = hook.command.split_first().ok_or("command is empty")?;
    let mut command =
        usagestat_core::process::command(program).map_err(|_| "program was not found")?;
    command.args(arguments);
    // Hooks inherit the daemon environment like custom provider commands,
    // except the dashboard management credential.
    command.env_remove("USAGESTAT_MANAGEMENT_KEY");
    command.envs(payload.environment(json));
    #[cfg(unix)]
    let result = usagestat_core::process::run_with_input(
        command,
        hook.timeout(),
        OUTPUT_LIMIT,
        format!("{json}\n").as_bytes(),
    );
    // Windows: the payload is available in USAGESTAT_PAYLOAD.
    #[cfg(not(unix))]
    let result = usagestat_core::process::run(command, hook.timeout(), OUTPUT_LIMIT);
    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(match output.status.code() {
            Some(code) => format!("exited with status {code}"),
            None => "terminated by a signal".into(),
        }),
        // A hook that exits without reading stdin is still a success.
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => Err(format!(
            "timed out after {}s and was stopped",
            hook.timeout().as_secs()
        )),
        Err(error) => Err(format!("{:?}", error.kind())),
    }
}

fn post(hook: &HookConfig, url: &str, json: &str) -> Result<(), String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let url = reqwest::Url::parse(url.trim()).map_err(|_| "url is invalid")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("url must use http or https".into());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(hook.timeout())
        .connect_timeout(hook.timeout().min(Duration::from_secs(10)))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("usagestat/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "HTTP client is unavailable")?;
    let mut request = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    for (name, value) in &hook.headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("header name {name:?} is invalid"))?;
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| format!("header {name} has an invalid value"))?;
        request = request.header(name, value);
    }
    let response = request.body(json.to_string()).send().map_err(describe)?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("endpoint returned HTTP {}", status.as_u16()))
    }
}

/// Error text without the URL (which may hold a token), plus its causes.
fn describe(error: reqwest::Error) -> String {
    let timed_out = error.is_timeout();
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    if timed_out {
        text.push_str(" (timed out)");
    }
    text
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use usagestat_core::hooks::HookEvent;

    fn payload() -> HookPayload {
        HookPayload::sample(HookEvent::QuotaThreshold, "claude", chrono::Utc::now())
    }

    fn hook(command: Vec<String>, url: Option<String>) -> HookConfig {
        HookConfig {
            enabled: true,
            events: vec!["quota_threshold".into()],
            command,
            url,
            timeout_sec: Some(5),
            ..HookConfig::default()
        }
    }

    fn script(dir: &std::path::Path, body: &str) -> String {
        let path = dir.join("hook.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn command_hook_receives_payload_on_stdin_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let program = script(
            dir.path(),
            r#"out="$1"; cat > "$out.stdin"; printf '%s|%s|%s|%s|%s' "$USAGESTAT_EVENT" "$USAGESTAT_PROVIDER" "$USAGESTAT_METRIC" "$USAGESTAT_USED_PERCENT" "$USAGESTAT_TEST" > "$out.env""#,
        );
        let payload = payload();
        deliver(
            &hook(vec![program, out.to_string_lossy().into_owned()], None),
            &payload,
        )
        .unwrap();
        let stdin = std::fs::read_to_string(dir.path().join("out.stdin")).unwrap();
        let received: HookPayload = serde_json::from_str(stdin.trim()).unwrap();
        assert_eq!(received, payload);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.env")).unwrap(),
            "quota_threshold|claude|Session|91|1"
        );
    }

    #[test]
    fn command_hook_failures_and_timeouts_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let failing = script(dir.path(), "exit 3");
        let error = deliver(&hook(vec![failing], None), &payload()).unwrap_err();
        assert!(error.contains("status 3"), "{error}");
        let slow = script(dir.path(), "exec sleep 30");
        let mut slow = hook(vec![slow], None);
        slow.timeout_sec = Some(1);
        let started = std::time::Instant::now();
        let error = deliver(&slow, &payload()).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let ignores_stdin = script(dir.path(), "exit 0");
        deliver(&hook(vec![ignores_stdin], None), &payload()).unwrap();
    }

    #[test]
    fn webhook_posts_json_with_headers_and_hides_secrets_on_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for status in ["204 No Content", "500 Internal Server Error"] {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut head = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                reader
                    .get_mut()
                    .write_all(format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n").as_bytes())
                    .unwrap();
                requests.push((head, body));
            }
            requests
        });
        let mut webhook = hook(Vec::new(), Some(format!("http://{address}/secret-path")));
        webhook
            .headers
            .insert("Authorization".into(), "Bearer header-secret".into());
        let payload = payload();
        deliver(&webhook, &payload).unwrap();
        let error = deliver(&webhook, &payload).unwrap_err();
        assert!(error.contains("HTTP 500"), "{error}");
        assert!(!error.contains("secret"), "{error}");
        let requests = server.join().unwrap();
        let (head, body) = &requests[0];
        assert!(head.starts_with("POST /secret-path HTTP/1.1"), "{head}");
        assert!(
            head.contains("authorization: Bearer header-secret"),
            "{head}"
        );
        assert!(head.contains("content-type: application/json"), "{head}");
        let received: HookPayload = serde_json::from_slice(body).unwrap();
        assert_eq!(received, payload);
    }

    #[test]
    fn invalid_hooks_are_rejected_before_running() {
        let error = deliver(&hook(Vec::new(), None), &payload()).unwrap_err();
        assert!(error.contains("command"), "{error}");
        let error = deliver(
            &hook(Vec::new(), Some("file:///etc/passwd".into())),
            &payload(),
        )
        .unwrap_err();
        assert!(error.contains("http"), "{error}");
    }
}
