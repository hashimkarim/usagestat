use usagestat_core::{
    AppConfig, LoadedProvider, MetricLine, NormalizedMetrics, ProgressFormat, ProviderSummary,
    UsageCache, UsageSnapshot, paths, usage_daily,
};

const DASHBOARD_HTML: &str = include_str!("dashboard.html");
const DASHBOARD_TRENDS_JS: &str = include_str!("dashboard-trends.js");
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::collections::HashMap;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use usagestat_plugins::{discover_providers, probe_provider};

mod cliproxy;
mod codex_usage;
mod control;
mod history;
mod http_request;
mod local_usage;
mod local_usage_cache;
mod pricing;
mod prometheus;
use usagestat_core::usage_daily::UsageModelDaily as ModelAggregate;

#[derive(Debug, Parser)]
#[command(name = "usagestatd", version)]
#[command(about = "Local agent usage polling daemon")]
struct Cli {
    /// Run the saved per-user service configuration.
    #[arg(long, value_name = "PATH")]
    service_settings: Option<PathBuf>,

    #[arg(long, default_value = "127.0.0.1:6736")]
    bind: String,

    #[arg(long)]
    refresh_sec: Option<u64>,

    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    #[arg(long = "plugin-dir", value_name = "DIR")]
    plugin_dirs: Vec<PathBuf>,

    /// Enable the T3 Code / CLIProxyAPI quota endpoint using a key file.
    /// Alternatively, set USAGESTAT_MANAGEMENT_KEY.
    #[arg(long, value_name = "PATH")]
    management_key_file: Option<PathBuf>,

    /// Separate credential for authenticated lifecycle control.
    #[arg(long, value_name = "PATH")]
    control_key_file: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct AppState {
    cache: UsageCache,
    providers: Vec<ProviderSummary>,
    identity: Option<JsonValue>,
    control: control::ControlApi,
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();
    let mut owner = None;
    if let Some(path) = &cli.service_settings {
        use usagestat_core::daemon_settings::{DaemonSettings, T3Mode};
        let settings = DaemonSettings::load(path)?.context("daemon settings are missing")?;
        let installation = settings
            .installation
            .context("daemon installation is not configured")?;
        cli.bind = installation.bind.to_string();
        cli.config = Some(installation.config);
        cli.plugin_dirs = installation.plugin_dirs;
        cli.management_key_file =
            (settings.t3_mode == T3Mode::Auto).then_some(installation.management_key_file);
        cli.control_key_file = Some(installation.control_key_file);
        owner = Some(installation.owner);
        // This is the binary's single-threaded entry, before logging, signals,
        // HTTP clients or provider workers can read environment variables.
        unsafe {
            std::env::remove_var("USAGESTAT_MANAGEMENT_KEY");
            for (name, value) in installation.environment {
                if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                    anyhow::bail!("invalid environment entry in daemon settings");
                }
                if name != "USAGESTAT_MANAGEMENT_KEY" {
                    std::env::set_var(name, value);
                }
            }
        }
    }
    env_logger::init();
    let shutdown = usagestat_core::signals::register()?;
    let control = control::ControlApi::load(cli.control_key_file.as_deref(), shutdown.clone())?;
    let management = cliproxy::ManagementApi::load(cli.management_key_file.as_deref())?;
    let config_path = match cli.config.clone() {
        Some(path) => path,
        None => paths::config_file()?,
    };
    let config = AppConfig::load_optional(&config_path)
        .with_context(|| format!("load config {}", config_path.display()))?;
    let refresh_sec = cli.refresh_sec.unwrap_or(config.refresh_sec);
    let plugin_dirs = paths::plugin_dirs(&config, &cli.plugin_dirs)?;
    let cache_path = paths::cache_file()?;
    let _profile_lock = usagestat_core::storage::exclusive_lock(
        &paths::data_dir()?.join("daemon.lock"),
    )
    .context(
        "acquire daemon profile lock; stop its existing daemon or choose another data directory",
    )?;
    let history_path = paths::data_dir()?.join("history.jsonl");
    let cache = UsageCache::load_optional(&cache_path)
        .with_context(|| format!("load cache {}", cache_path.display()))?;

    let state = Arc::new(Mutex::new(AppState {
        cache,
        providers: Vec::new(),
        identity: Some(json!({
            "application": "usagestat", "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(), "profile": paths::app_dir_name(), "owner": owner,
        })),
        control,
    }));
    let refresh_flag = Arc::new(AtomicBool::new(false));

    // A conflicting listener must fail before any provider is started.
    let listener =
        TcpListener::bind(&cli.bind).with_context(|| format!("bind daemon at {}", cli.bind))?;
    let poller = start_poller(
        Arc::clone(&state),
        Arc::clone(&refresh_flag),
        config,
        plugin_dirs,
        cache_path,
        history_path,
        refresh_sec,
        Arc::clone(&shutdown),
    );
    // Build daily model history away from the HTTP/UI path. Per-file caches
    // survive restarts; subsequent passes only parse changed transcripts.
    start_usage_summarizer(Arc::clone(&state), Arc::clone(&shutdown));
    let result = serve(
        listener,
        state,
        refresh_flag,
        Arc::new(management),
        Arc::clone(&shutdown),
    );
    shutdown.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !poller.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if poller.is_finished() {
        let _ = poller.join();
    } else {
        log::warn!("provider transport did not finish within the shutdown deadline");
    }
    result
}

fn start_usage_summarizer(state: Arc<Mutex<AppState>>, shutdown: Arc<AtomicBool>) {
    thread::spawn(move || {
        while !shutdown.load(Ordering::Relaxed) {
            let providers: Vec<_> = state
                .lock()
                .expect("app state poisoned")
                .providers
                .iter()
                .filter(|provider| {
                    provider.enabled && matches!(provider.id.as_str(), "codex" | "claude")
                })
                .map(|provider| provider.id.clone())
                .collect();
            for provider in &providers {
                if shutdown.load(Ordering::Relaxed) {
                    return;
                }
                if let Err(error) = local_usage_cache::report(provider, "daily") {
                    log::warn!("local daily summarization for {provider} failed: {error}");
                }
            }
            for _ in 0..if providers.is_empty() { 1 } else { 60 } {
                if shutdown.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    });
}

fn start_poller(
    state: Arc<Mutex<AppState>>,
    refresh_flag: Arc<AtomicBool>,
    config: AppConfig,
    plugin_dirs: Vec<PathBuf>,
    cache_path: PathBuf,
    history_path: PathBuf,
    refresh_sec: u64,
    shutdown: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !shutdown.load(Ordering::SeqCst) {
            let mut providers = discover_providers(&plugin_dirs);
            sort_providers(&mut providers, &config);
            let summaries = provider_summaries(&providers, &config);
            state.lock().expect("app state poisoned").providers = summaries;

            for provider in &providers {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                if config.is_enabled(&provider.manifest.id, provider.manifest.enabled_by_default) {
                    let source = config.source_mode(&provider.manifest.id);
                    let token = usagestat_core::process::CancellationToken::with_interrupt(Some(
                        Arc::clone(&shutdown),
                    ));
                    let snapshot = usagestat_core::process::with_cancellation(token, || {
                        probe_provider(
                            provider,
                            source,
                            config.provider_config(&provider.manifest.id),
                        )
                    });
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    let record = history_record_from_snapshot(&snapshot);
                    let mut guard = state.lock().expect("app state poisoned");
                    guard.cache.upsert(snapshot);
                    if let Err(e) = guard.cache.save(&cache_path) {
                        log::warn!("failed to save usage cache: {e}");
                    }
                    if let Err(e) = append_history_record(&history_path, &record) {
                        log::warn!("failed to append usage history: {e}");
                    }
                }
            }

            // Sleep until next cycle, but wake early if refresh is requested.
            refresh_flag.store(false, Ordering::Relaxed);
            let deadline = Instant::now() + Duration::from_secs(refresh_sec.max(1));
            loop {
                if shutdown.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
                if refresh_flag.load(Ordering::Relaxed) {
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
        }
    })
}

fn serve(
    listener: TcpListener,
    state: Arc<Mutex<AppState>>,
    refresh_flag: Arc<AtomicBool>,
    management: Arc<cliproxy::ManagementApi>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    log::info!("listening on http://{}", listener.local_addr()?);

    while !shutdown.load(Ordering::SeqCst) {
        let state = Arc::clone(&state);
        let flag = Arc::clone(&refresh_flag);
        let management = Arc::clone(&management);
        match listener.accept() {
            Ok((stream, _)) => {
                thread::spawn(move || handle_connection(stream, state, flag, management));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(e) => return Err(e).context("accept daemon connection"),
        }
    }

    Ok(())
}

fn handle_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<AppState>>,
    refresh_flag: Arc<AtomicBool>,
    management: Arc<cliproxy::ManagementApi>,
) {
    // Winsock accept inherits the listener's nonblocking mode. A request can
    // arrive in several packets; keep this bounded handler blocking so partial
    // headers are read completely instead of treating WouldBlock as bad HTTP.
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut shutdown_after_response = None;
    let response = match http_request::read_request(&mut stream) {
        Ok(request) => {
            let controlled = state
                .lock()
                .expect("app state poisoned")
                .control
                .route(&request);
            controlled
                .map(|reply| {
                    shutdown_after_response = reply.shutdown;
                    reply.response
                })
                .or_else(|| management.route(&request, &state))
                .unwrap_or_else(|| {
                    route(
                        &request.method,
                        &request.path,
                        &request.query,
                        &state,
                        &refresh_flag,
                    )
                })
        }
        Err(_) => response_json(400, "Bad Request", r#"{"error":"invalid_request"}"#),
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    if let Some(shutdown) = shutdown_after_response {
        shutdown.store(true, Ordering::SeqCst);
    }
}

fn route(
    method: &str,
    path: &str,
    query: &str,
    state: &Arc<Mutex<AppState>>,
    refresh_flag: &Arc<AtomicBool>,
) -> String {
    if method == "OPTIONS" {
        return response_no_content();
    }

    // POST /v1/refresh — trigger immediate re-probe
    if method == "POST" && path == "/v1/refresh" {
        refresh_flag.store(true, Ordering::Relaxed);
        return response_json(200, "OK", r#"{"status":"refresh_scheduled"}"#);
    }

    if method != "GET" {
        return response_json(
            405,
            "Method Not Allowed",
            r#"{"error":"method_not_allowed"}"#,
        );
    }

    if path == "/health" {
        let mut body = state
            .lock()
            .expect("app state poisoned")
            .identity
            .clone()
            .unwrap_or_else(|| json!({}));
        body["status"] = json!("ok");
        return response_json(200, "OK", &body.to_string());
    }

    if path == "/v1/capabilities" {
        let providers = state.lock().expect("app state poisoned").providers.clone();
        let body = serde_json::to_string(&usagestat_core::capabilities::current(&providers))
            .expect("serialize capabilities");
        return response_json(200, "OK", &body);
    }

    if path == "/dashboard" || path == "/" {
        return response_html(200, "OK", DASHBOARD_HTML);
    }

    if path == "/dashboard/trends.js" {
        return response_text(
            200,
            "OK",
            "text/javascript; charset=utf-8",
            DASHBOARD_TRENDS_JS,
        );
    }

    if path == "/v1/providers" {
        let providers = state.lock().expect("app state poisoned").providers.clone();
        let body = serde_json::to_string(&providers).unwrap_or_else(|_| "[]".into());
        return response_json(200, "OK", &body);
    }

    if let Some(id) = path.strip_prefix("/v1/icons/") {
        let providers = state.lock().expect("app state poisoned").providers.clone();
        // Resolve only registered assets, never an arbitrary request-supplied path.
        let icon = providers
            .iter()
            .find(|provider| provider.id == id)
            .and_then(|provider| provider.icon.as_ref());
        if let Some(icon) = icon {
            let file = icon.color_path.as_ref().or(icon.path.as_ref());
            if let Some(file) = file.filter(|file| file.ends_with(".svg")) {
                if std::fs::metadata(file)
                    .map(|meta| meta.len() <= 128 * 1024)
                    .unwrap_or(false)
                {
                    if let Ok(svg) = std::fs::read_to_string(file) {
                        return response_text(200, "OK", "image/svg+xml", &svg);
                    }
                }
            }
        }
        return response_json(404, "Not Found", r#"{"error":"icon_not_found"}"#);
    }

    if path == "/v1/usage" {
        let guard = state.lock().expect("app state poisoned");
        let snapshots = ordered_snapshots(&guard);
        let body = serde_json::to_string(&snapshots).unwrap_or_else(|_| "[]".into());
        return response_json(200, "OK", &body);
    }

    if path == "/metrics" {
        let guard = state.lock().expect("app state poisoned");
        let snapshots = ordered_snapshots(&guard);
        let enabled: Vec<_> = snapshots
            .iter()
            .filter(|snapshot| {
                guard
                    .providers
                    .iter()
                    .any(|provider| provider.enabled && provider.id == snapshot.provider_id)
            })
            .collect();
        return response_text(
            200,
            "OK",
            "text/plain; version=0.0.4; charset=utf-8",
            &prometheus::render(&enabled),
        );
    }

    if path == "/v1/history" || path == "/v1/history/quota" {
        return history::serve(None, query);
    }
    if let Some(provider_id) = path.strip_prefix("/v1/history/quota/") {
        return history::serve(Some(provider_id), query);
    }

    if path == "/v1/history/daily" {
        if query.split('&').any(|pair| pair == "includeSources=true") {
            return serve_saved_daily_sources(None);
        }
        return serve_all_saved_daily_history();
    }

    if let Some(provider_id) = path.strip_prefix("/v1/history/models/") {
        return match usage_daily::selected_model_daily_rows(provider_id) {
            Ok(rows) => response_json(
                200,
                "OK",
                &json!({"daily":rows,"timeZone":"UTC"}).to_string(),
            ),
            Err(_) => response_json(
                500,
                "Internal Server Error",
                r#"{"error":"history_unavailable"}"#,
            ),
        };
    }

    if let Some(provider_id) = path.strip_prefix("/v1/history/daily/") {
        if query.split('&').any(|pair| pair == "includeSources=true") {
            return serve_saved_daily_sources(Some(provider_id));
        }
        if let Some((id, period)) = provider_id.split_once('/') {
            match usage_daily::selected_daily_rows(id) {
                Ok(mut rows) => {
                    if let Err(error) =
                        usage_daily::filter_period(&mut rows, period, Utc::now().date_naive())
                    {
                        return response_json(
                            400,
                            "Bad Request",
                            &json!({"error": error}).to_string(),
                        );
                    }
                    return response_json(
                        200,
                        "OK",
                        &json!({"daily":rows,"period":period,"timeZone":"UTC"}).to_string(),
                    );
                }
                Err(_) => {
                    return response_json(
                        500,
                        "Internal Server Error",
                        r#"{"error":"history_unavailable"}"#,
                    );
                }
            }
        }
        return serve_saved_usage_report(provider_id, "daily");
    }

    if let Some(provider_id) = path.strip_prefix("/v1/cost/") {
        return serve_cost(provider_id);
    }

    if let Some(rest) = path
        .strip_prefix("/v1/local-usage/")
        .or_else(|| path.strip_prefix("/v1/ccusage/"))
    {
        return serve_local_usage_report(rest, query);
    }

    if let Some(provider_id) = path.strip_prefix("/v1/history/") {
        return history::serve(Some(provider_id), query);
    }

    if let Some(provider_id) = path.strip_prefix("/v1/usage/") {
        let guard = state.lock().expect("app state poisoned");
        return match guard.cache.get(provider_id) {
            Some(snap) => {
                let body = serde_json::to_string(snap).unwrap_or_else(|_| "{}".into());
                response_json(200, "OK", &body)
            }
            None => response_json(404, "Not Found", r#"{"error":"provider_not_found"}"#),
        };
    }

    response_json(404, "Not Found", r#"{"error":"not_found"}"#)
}

fn response_json(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Connection: close\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type, Authorization, X-Management-Key\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn serve_cost(provider_id: &str) -> String {
    if let Some(response) = saved_cost_response(provider_id) {
        return response;
    }

    let canonical = match provider_id {
        "claude" | "anthropic" => "claude",
        "codex" | "openai" | "chatgpt" => "codex",
        _ => {
            return serve_saved_cost(provider_id);
        }
    };
    match local_usage_cache::report(canonical, "daily") {
        Ok(_) => saved_cost_response(canonical).unwrap_or_else(|| response_json(200, "OK",
            &json!({"provider":canonical,"currency":"USD","daily":[],"totals":ModelAggregate::default()}).to_string())),
        Err(error) if error.is::<usagestat_core::provider_paths::ProviderPathError>() => {
            provider_path_error_response()
        }
        Err(_) => serve_saved_cost(provider_id),
    }
}

fn serve_local_usage_report(rest: &str, query: &str) -> String {
    let mut parts = rest.split('/');
    let provider_id = parts.next().unwrap_or_default();
    let report = parts.next().unwrap_or("daily");
    if parts.next().is_some() {
        return response_json(404, "Not Found", r#"{"error":"not_found"}"#);
    }
    let limit = if query.is_empty() {
        None
    } else {
        let limit = query
            .strip_prefix("limit=")
            .and_then(|limit| limit.parse::<usize>().ok())
            .filter(|limit| (1..=1000).contains(limit));
        if limit.is_none() || !matches!(report, "session" | "blocks") {
            return response_json(
                400,
                "Bad Request",
                r#"{"error":"limit must be 1-1000 for session or blocks reports"}"#,
            );
        }
        limit
    };
    let provider = match provider_id {
        "claude" | "anthropic" => "claude",
        "codex" | "openai" | "chatgpt" => "codex",
        _ => {
            return serve_saved_usage_report(provider_id, report);
        }
    };
    // An explicitly selected missing profile must not return a saved reading
    // from an earlier account. This only validates roots; it does not scan logs.
    let roots = match provider {
        "codex" => usagestat_core::provider_paths::codex_usage_roots(),
        _ => usagestat_core::provider_paths::claude_usage_roots(),
    };
    if roots.is_err() {
        return provider_path_error_response();
    }
    if matches!(report, "daily" | "weekly" | "monthly") {
        if let Some(response) = saved_usage_report_response(provider_id, report) {
            return response;
        }
    }
    if !matches!(
        report,
        "daily" | "weekly" | "monthly" | "session" | "blocks" | "models"
    ) {
        return response_json(
            400,
            "Bad Request",
            r#"{"error":{"code":"BAD_REPORT","message":"Unsupported usage report"}}"#,
        );
    }

    match local_usage_cache::report_limited(provider, report, limit) {
        Ok(body) => response_json(200, "OK", &body),
        Err(e) => {
            if e.is::<usagestat_core::provider_paths::ProviderPathError>() {
                return provider_path_error_response();
            }
            log::warn!("local usage report failed: {e}");
            serve_saved_usage_report(provider_id, report)
        }
    }
}

fn provider_path_error_response() -> String {
    response_json(
        200,
        "OK",
        r#"{"error":{"code":"LOCAL_USAGE_PATH_UNAVAILABLE","message":"Provider profile directory unavailable; check CLAUDE_CONFIG_DIR or CODEX_HOME"}}"#,
    )
}

fn serve_saved_usage_report(provider_id: &str, report: &str) -> String {
    saved_usage_report_response(provider_id, report).unwrap_or_else(|| {
        response_json(
            200,
            "OK",
            r#"{"error":{"code":"UNAVAILABLE","message":"Saved daily usage is not available for this provider"}}"#,
        )
    })
}

fn serve_saved_daily_sources(provider_path: Option<&str>) -> String {
    let (provider_id, period) = match provider_path.and_then(|path| path.split_once('/')) {
        Some((id, period)) => (Some(id), Some(period)),
        None => (provider_path, None),
    };
    let rows = match usage_daily::selected_daily_rows_with_sources(provider_id) {
        Ok(rows) => rows,
        Err(_) => {
            return response_json(
                500,
                "Internal Server Error",
                r#"{"error":"history_unavailable"}"#,
            );
        }
    };
    let mut body = json!({"daily":rows});
    if let Some(period) = period {
        let today = Utc::now().date_naive();
        let start = match usage_daily::period_start(period, today) {
            Ok(start) => start,
            Err(error) => {
                return response_json(400, "Bad Request", &json!({"error":error}).to_string());
            }
        };
        body["daily"].as_array_mut().unwrap().retain(|row| {
            row["date"]
                .as_str()
                .and_then(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                .is_some_and(|day| day <= today && start.is_none_or(|start| day >= start))
        });
        body["period"] = json!(period);
        body["timeZone"] = json!("UTC");
    } else if provider_id.is_some() && body["daily"].as_array().is_some_and(Vec::is_empty) {
        return response_json(
            200,
            "OK",
            r#"{"error":{"code":"UNAVAILABLE","message":"Saved daily usage is not available for this provider"}}"#,
        );
    }
    response_json(200, "OK", &body.to_string())
}

fn serve_all_saved_daily_history() -> String {
    match usage_daily::all_selected_daily_rows() {
        Ok(rows) => {
            let body =
                serde_json::to_string(&json!({ "daily": rows })).unwrap_or_else(|_| "{}".into());
            response_json(200, "OK", &body)
        }
        Err(e) => {
            log::warn!("saved daily history failed: {e}");
            response_json(
                200,
                "OK",
                r#"{"error":{"code":"UNAVAILABLE","message":"Saved daily usage history unavailable"}}"#,
            )
        }
    }
}

fn saved_usage_report_response(provider_id: &str, report: &str) -> Option<String> {
    match usage_daily::report_json(provider_id, report) {
        Ok(value) => {
            if value.get("error").is_some() {
                return None;
            }
            let body = serde_json::to_string(&value).unwrap_or_else(|_| "{}".into());
            Some(response_json(200, "OK", &body))
        }
        Err(e) => {
            log::warn!("saved daily usage report failed: {e}");
            None
        }
    }
}

fn serve_saved_cost(provider_id: &str) -> String {
    saved_cost_response(provider_id).unwrap_or_else(|| {
        response_json(
            200,
            "OK",
            r#"{"error":{"code":"UNSUPPORTED","message":"Cost data not available for this provider"}}"#,
        )
    })
}

fn saved_cost_response(provider_id: &str) -> Option<String> {
    match usage_daily::selected_daily_rows(provider_id) {
        Ok(rows) if !rows.is_empty() => {
            let totals = rows.iter().fold(ModelAggregate::default(), |mut acc, row| {
                acc.add(&row.metrics());
                acc
            });
            let body = serde_json::to_string(&json!({
                "provider": provider_id,
                "currency": "USD",
                "daily": rows,
                "totals": totals,
                "costKnown": rows.iter().all(|row| row.cost_known),
                "tokensKnown": rows.iter().all(|row| row.tokens_known),
            }))
            .unwrap_or_else(|_| "{}".into());
            Some(response_json(200, "OK", &body))
        }
        Ok(_) => None,
        Err(e) => {
            log::warn!("saved daily cost report failed: {e}");
            None
        }
    }
}

fn response_html(status: u16, reason: &str, body: &str) -> String {
    response_text(status, reason, "text/html; charset=utf-8", body)
}

fn response_text(status: u16, reason: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Connection: close\r\n\
         Content-Type: {content_type}\r\n\
         Cache-Control: no-store\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn response_no_content() -> String {
    "HTTP/1.1 204 No Content\r\n\
     Connection: close\r\n\
     Access-Control-Allow-Origin: *\r\n\
     Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
     Access-Control-Allow-Headers: Content-Type, Authorization, X-Management-Key\r\n\r\n"
        .to_string()
}

fn ordered_snapshots(state: &AppState) -> Vec<UsageSnapshot> {
    let mut snapshots = Vec::new();
    for provider in &state.providers {
        if !provider.enabled {
            continue;
        }
        if let Some(snapshot) = state.cache.get(&provider.id) {
            snapshots.push(snapshot.clone());
        }
    }
    let seen: std::collections::HashSet<String> = snapshots
        .iter()
        .map(|snapshot| snapshot.provider_id.clone())
        .collect();
    for snapshot in state.cache.list() {
        if !seen.contains(&snapshot.provider_id) {
            snapshots.push(snapshot);
        }
    }
    snapshots
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotRecord {
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
    #[serde(default, alias = "cache_read_tokens")]
    cache_read_tokens: Option<u64>,
    #[serde(default, alias = "cache_creation_tokens")]
    cache_creation_tokens: Option<u64>,
    #[serde(default, alias = "total_tokens")]
    total_tokens: Option<u64>,
    cost: Option<f64>,
    #[serde(alias = "reset_time")]
    reset_time: Option<String>,
    #[serde(default)]
    progress: Vec<HistoryProgressRecord>,
    #[serde(default)]
    text: Vec<HistoryTextRecord>,
    #[serde(default)]
    badges: Vec<HistoryBadgeRecord>,
    #[serde(default)]
    charts: Vec<HistoryBarChartRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryProgressRecord {
    label: String,
    used: f64,
    limit: f64,
    percent: Option<f64>,
    format: String,
    suffix: Option<String>,
    resets_at: Option<String>,
    period_duration_ms: Option<u64>,
    detail: Option<String>,
    color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryTextRecord {
    label: String,
    value: String,
    color: Option<String>,
    subtitle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryBadgeRecord {
    label: String,
    text: String,
    color: Option<String>,
    subtitle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryBarChartRecord {
    label: String,
    points: Vec<HistoryBarChartPoint>,
    note: Option<String>,
    color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryBarChartPoint {
    label: String,
    value: f64,
    value_label: Option<String>,
}

fn history_record_from_snapshot(snapshot: &UsageSnapshot) -> SnapshotRecord {
    let metrics = NormalizedMetrics::from_snapshot(snapshot);
    let progress = progress_history(snapshot);
    let text = text_history(snapshot);
    let badges = badge_history(snapshot);
    let charts = bar_chart_history(snapshot);
    let token_breakdown = token_breakdown(snapshot, &metrics);
    SnapshotRecord {
        ts: snapshot.fetched_at.to_rfc3339(),
        provider_id: snapshot.provider_id.clone(),
        display_name: snapshot.display_name.clone(),
        plan: snapshot.plan.clone(),
        primary_percent: metrics.primary_percent,
        input_tokens: token_breakdown.input.or(metrics.input_tokens),
        output_tokens: token_breakdown.output.or(metrics.output_tokens),
        cache_read_tokens: token_breakdown.cache_read,
        cache_creation_tokens: token_breakdown.cache_creation,
        total_tokens: token_breakdown.total.or_else(|| {
            Some(
                [
                    token_breakdown.input.or(metrics.input_tokens),
                    token_breakdown.output.or(metrics.output_tokens),
                    token_breakdown.cache_read,
                    token_breakdown.cache_creation,
                ]
                .into_iter()
                .flatten()
                .sum(),
            )
            .filter(|total| *total > 0)
        }),
        cost: metrics.cost,
        reset_time: metrics.reset_time,
        progress,
        text,
        badges,
        charts,
    }
}

fn progress_history(snapshot: &UsageSnapshot) -> Vec<HistoryProgressRecord> {
    snapshot
        .metrics
        .iter()
        .filter_map(|metric| match metric {
            MetricLine::Progress {
                label,
                used,
                limit,
                format,
                resets_at,
                period_duration_ms,
                detail,
                color,
            } => {
                let (format_name, suffix) = match format {
                    ProgressFormat::Percent => ("percent".to_string(), None),
                    ProgressFormat::Dollars => ("dollars".to_string(), None),
                    ProgressFormat::Count { suffix } => ("count".to_string(), Some(suffix.clone())),
                };
                Some(HistoryProgressRecord {
                    label: label.clone(),
                    used: *used,
                    limit: *limit,
                    percent: (*limit > 0.0).then(|| (*used / *limit * 100.0).clamp(0.0, 100.0)),
                    format: format_name,
                    suffix,
                    resets_at: resets_at.map(|dt| dt.to_rfc3339()),
                    period_duration_ms: *period_duration_ms,
                    detail: detail.clone(),
                    color: color.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

fn text_history(snapshot: &UsageSnapshot) -> Vec<HistoryTextRecord> {
    snapshot
        .metrics
        .iter()
        .filter_map(|metric| match metric {
            MetricLine::Text {
                label,
                value,
                color,
                subtitle,
            } => Some(HistoryTextRecord {
                label: label.clone(),
                value: value.clone(),
                color: color.clone(),
                subtitle: subtitle.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn badge_history(snapshot: &UsageSnapshot) -> Vec<HistoryBadgeRecord> {
    snapshot
        .metrics
        .iter()
        .filter_map(|metric| match metric {
            MetricLine::Badge {
                label,
                text,
                color,
                subtitle,
            } => Some(HistoryBadgeRecord {
                label: label.clone(),
                text: text.clone(),
                color: color.clone(),
                subtitle: subtitle.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn bar_chart_history(snapshot: &UsageSnapshot) -> Vec<HistoryBarChartRecord> {
    snapshot
        .metrics
        .iter()
        .filter_map(|metric| match metric {
            MetricLine::BarChart {
                label,
                points,
                note,
                color,
            } => Some(HistoryBarChartRecord {
                label: label.clone(),
                points: points
                    .iter()
                    .map(|point| HistoryBarChartPoint {
                        label: point.label.clone(),
                        value: point.value,
                        value_label: point.value_label.clone(),
                    })
                    .collect(),
                note: note.clone(),
                color: color.clone(),
            }),
            _ => None,
        })
        .collect()
}

#[derive(Default)]
struct TokenBreakdown {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    total: Option<u64>,
}

fn token_breakdown(snapshot: &UsageSnapshot, metrics: &NormalizedMetrics) -> TokenBreakdown {
    let mut out = TokenBreakdown {
        input: metrics.input_tokens,
        output: metrics.output_tokens,
        ..TokenBreakdown::default()
    };

    for metric in &snapshot.metrics {
        match metric {
            MetricLine::Progress {
                label,
                used,
                format,
                ..
            } => {
                if matches!(format, ProgressFormat::Count { .. }) {
                    assign_token_value(&mut out, label, *used);
                }
            }
            MetricLine::Text { label, value, .. } => {
                if let Some(value) = parse_u64_loose(value) {
                    assign_token_u64(&mut out, label, value);
                }
            }
            MetricLine::Badge { label, text, .. } => {
                if let Some(value) = parse_u64_loose(text) {
                    assign_token_u64(&mut out, label, value);
                }
            }
            MetricLine::BarChart { .. } => {}
        }
    }

    let total = [out.input, out.output, out.cache_read, out.cache_creation]
        .into_iter()
        .flatten()
        .sum::<u64>();
    if total > 0 {
        out.total = Some(total);
    }
    out
}

fn assign_token_value(out: &mut TokenBreakdown, label: &str, value: f64) {
    if value.is_finite() && value >= 0.0 {
        assign_token_u64(out, label, value.min(u64::MAX as f64) as u64);
    }
}

fn assign_token_u64(out: &mut TokenBreakdown, label: &str, value: u64) {
    let label = label.to_lowercase();
    if !(label.contains("token") || label.contains("tok")) {
        return;
    }
    if label.contains("cache") && (label.contains("read") || label.contains("hit")) {
        out.cache_read = Some(value);
    } else if label.contains("cache")
        && (label.contains("write") || label.contains("creation") || label.contains("create"))
    {
        out.cache_creation = Some(value);
    } else if label.contains("output") || label.contains("completion") {
        out.output = Some(value);
    } else if label.contains("input") || label.contains("prompt") {
        out.input = Some(value);
    } else if label.contains("total") {
        out.total = Some(value);
    }
}

fn parse_u64_loose(value: &str) -> Option<u64> {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn append_history_record(path: &std::path::Path, record: &SnapshotRecord) -> Result<()> {
    let mut line = serde_json::to_vec(record).context("serialize history record")?;
    line.push(10);
    usagestat_core::storage::append_private(path, &line)
        .with_context(|| format!("append history {}", path.display()))
}

fn provider_summaries(providers: &[LoadedProvider], config: &AppConfig) -> Vec<ProviderSummary> {
    providers
        .iter()
        .map(|p| ProviderSummary {
            id: p.manifest.id.clone(),
            name: p.manifest.name.clone(),
            enabled: config.is_enabled(&p.manifest.id, p.manifest.enabled_by_default),
            supported_modes: p.manifest.supported_modes.clone(),
            auto_mode: p.manifest.auto_mode.clone(),
            web_url: p.manifest.web_url.clone(),
            status_page_url: p.manifest.resolved_status_page_url(),
            usage_dashboard_url: p.manifest.resolved_usage_dashboard_url(),
            icon: p.manifest.resolved_icon(&p.dir),
        })
        .collect()
}

fn sort_providers(providers: &mut [LoadedProvider], config: &AppConfig) {
    let order: HashMap<&str, usize> = config
        .providers
        .iter()
        .enumerate()
        .map(|(index, provider)| (provider.id.as_str(), index))
        .collect();
    providers.sort_by(|a, b| {
        let ao = order.get(a.manifest.id.as_str()).copied();
        let bo = order.get(b.manifest.id.as_str()).copied();
        match (ao, bo) {
            (Some(a_index), Some(b_index)) => a_index.cmp(&b_index),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.manifest.id.cmp(&b.manifest.id),
        }
    });
}
