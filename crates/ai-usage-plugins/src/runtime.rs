use crate::host_api;
use chrono::{DateTime, Utc};
use rquickjs::{Array, Context, Ctx, Object, Runtime, Value, context::EvalOptions, promise::MaybePromise};
use usagestat_core::{
    BarChartPoint, LoadedProvider, MetricLine, ProgressFormat, ProviderConfig, ProviderManifest,
    UsageSnapshot, paths,
};

pub fn probe_provider(
    provider: &LoadedProvider,
    source_mode: &str,
    provider_config: Option<&ProviderConfig>,
) -> UsageSnapshot {
    if let Some(error) = provider.manifest.check_mode(source_mode) {
        return UsageSnapshot::error(
            provider.manifest.id.clone(),
            provider.manifest.name.clone(),
            format!("unsupported: {error}"),
        );
    }

    let fallback = || {
        UsageSnapshot::error(
            provider.manifest.id.clone(),
            provider.manifest.name.clone(),
            "plugin runtime error",
        )
    };

    let Ok(rt) = Runtime::new() else {
        return fallback();
    };
    let cancellation = usagestat_core::process::current_cancellation();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    rt.set_memory_limit(128 * 1024 * 1024);
    rt.set_interrupt_handler(Some(Box::new(move || std::time::Instant::now() >= deadline
        || cancellation.as_ref().is_some_and(|value| value.is_cancelled()))));
    let Ok(ctx) = Context::full(&rt) else {
        return fallback();
    };

    ctx.with(|ctx| {
        run_in_context(ctx, provider, source_mode, provider_config).unwrap_or_else(|message| {
            UsageSnapshot::error(
                provider.manifest.id.clone(),
                provider.manifest.name.clone(),
                message,
            )
        })
    })
}

fn run_in_context(
    ctx: Ctx<'_>,
    provider: &LoadedProvider,
    source_mode: &str,
    provider_config: Option<&ProviderConfig>,
) -> Result<UsageSnapshot, String> {
    inject_context(&ctx, &provider.manifest, source_mode, provider_config)
        .map_err(|_| "host api injection failed".to_string())?;

    ctx.eval::<(), _>(include_str!("bundled_provider.js").as_bytes())
        .map_err(|_| "bundled provider adapter injection failed".to_string())?;

    // A stable source name makes QuickJS exceptions actionable without exposing
    // the user's installation path or dumping plugin source/credentials.
    let mut eval_options = EvalOptions::default();
    eval_options.filename = Some(format!("{}/{}", provider.manifest.id, provider.manifest.entry));
    ctx.eval_with_options::<(), _>(provider.entry_script.as_bytes(), eval_options)
        .map_err(|error| plugin_error(&ctx, error, "script evaluation"))?;

    let globals = ctx.globals();
    let plugin_obj: Object = globals
        .get("__usagestat_plugin")
        .or_else(|_| globals.get("__ai_usage_plugin"))
        .or_else(|_| globals.get("__openusage_plugin"))
        .map_err(|_| "missing plugin export".to_string())?;
    let probe_fn: rquickjs::Function = plugin_obj
        .get("probe")
        .map_err(|_| "missing probe()".to_string())?;
    let probe_ctx: Value = globals
        .get("__usagestat_ctx")
        .unwrap_or_else(|_| Value::new_undefined(ctx.clone()));
    let result: Object = probe_fn.call::<_, MaybePromise>((probe_ctx,))
        .and_then(|value| value.finish())
        .map_err(|error| plugin_error(&ctx, error, "probe"))?;

    let display_name = result
        .get::<_, String>("displayName")
        .unwrap_or_else(|_| provider.manifest.name.clone());
    let source = result.get::<_, String>("source").ok();
    let plan = result.get::<_, String>("plan").ok();
    let metrics = parse_metrics(&result)?;
    let now = Utc::now();
    let fetched_at = result
        .get::<_, String>("fetchedAt")
        .ok()
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc))
        .filter(|value| value.timestamp() > 0 && *value <= now)
        .unwrap_or(now);
    let state = result
        .get::<_, String>("state")
        .ok()
        .and_then(|state| serde_json::from_value(serde_json::Value::String(state)).ok())
        .unwrap_or(if metrics.is_empty() {
            usagestat_core::model::ProviderState::NoData
        } else {
            usagestat_core::model::ProviderState::Ready
        });

    Ok(UsageSnapshot {
        provider_id: provider.manifest.id.clone(),
        display_name,
        source,
        plan,
        metrics,
        fetched_at,
        status_page_url: None,
        pace: None,
        state: Some(state),
    })
}

fn plugin_error(ctx: &Ctx<'_>, error: rquickjs::Error, phase: &str) -> String {
    if !error.is_exception() {
        return format!("Plugin {phase} failed: {error}");
    }
    extract_error_string(ctx)
}

fn extract_error_string(ctx: &Ctx<'_>) -> String {
    let exc = ctx.catch();
    if exc.is_null() || exc.is_undefined() {
        return "The plugin failed.".to_string();
    }
    if let Some(value) = exc.as_string() {
        let message = value.to_string().unwrap_or_default();
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(value) = exc.as_object() {
        let message = value.get::<_, String>("message").unwrap_or_default();
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            if let Ok(code) = value.get::<_, String>("code") {
                if serde_json::from_value::<usagestat_core::model::ProviderState>(
                    serde_json::Value::String(code.clone()),
                )
                .is_ok_and(|state| state != usagestat_core::model::ProviderState::Ready)
                {
                    return format!("{code}: {trimmed}");
                }
            }
            let name = value.get::<_, String>("name").unwrap_or_default();
            // Ordinary strings and structured provider errors retain their
            // actionable auth/state messages. Native JS errors also need their
            // type and source location (QuickJS often says only "not a function").
            if !name.is_empty() {
                let stack = value.get::<_, String>("stack").unwrap_or_default();
                let frame = stack.lines().map(str::trim).find(|line| line.starts_with("at "));
                return match frame {
                    Some(frame) => format!("{name}: {trimmed} ({frame})"),
                    None => format!("{name}: {trimmed}"),
                };
            }
            return trimmed.to_string();
        }
    }
    let debug = format!("{exc:?}");
    if !debug.trim().is_empty() {
        return debug;
    }
    "The plugin failed.".to_string()
}

fn inject_context(
    ctx: &Ctx<'_>,
    manifest: &ProviderManifest,
    source_mode: &str,
    provider_config: Option<&ProviderConfig>,
) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    let app = Object::new(ctx.clone())?;
    app.set("version", env!("CARGO_PKG_VERSION"))?;
    app.set("platform", std::env::consts::OS)?;
    let app_data_dir = paths::data_dir()
        .map_err(|error| rquickjs::Exception::throw_message(ctx, &error.to_string()))?;
    let plugin_data_dir = app_data_dir.join("plugins").join(&manifest.id);
    usagestat_core::storage::private_directory(&plugin_data_dir)
        .map_err(|error| rquickjs::Exception::throw_message(ctx, &error.to_string()))?;
    app.set("appDataDir", app_data_dir.to_string_lossy().to_string())?;
    app.set(
        "pluginDataDir",
        plugin_data_dir.to_string_lossy().to_string(),
    )?;

    let probe_ctx = Object::new(ctx.clone())?;
    probe_ctx.set("nowIso", Utc::now().to_rfc3339())?;
    probe_ctx.set("sourceMode", source_mode)?;
    if let Some(web_url) = &manifest.web_url {
        probe_ctx.set("webUrl", web_url.as_str())?;
    }
    if let Some(provider_config) = provider_config {
        let provider_obj = Object::new(ctx.clone())?;
        provider_obj.set("id", provider_config.id.as_str())?;
        if let Some(instance_id) = &provider_config.instance_id {
            provider_obj.set("instanceId", instance_id.as_str())?;
        }
        if let Some(display_name) = &provider_config.display_name {
            provider_obj.set("displayName", display_name.as_str())?;
        }
        if let Some(api_key) = &provider_config.api_key {
            provider_obj.set("apiKey", api_key.as_str())?;
        }
        if let Some(cookie_header) = &provider_config.cookie_header {
            provider_obj.set("cookieHeader", cookie_header.as_str())?;
        }
        if let Some(region) = &provider_config.region {
            provider_obj.set("region", region.as_str())?;
        }
        if let Some(workspace_id) = &provider_config.workspace_id {
            provider_obj.set("workspaceId", workspace_id.as_str())?;
        }
        if !provider_config.settings.is_empty() {
            let settings_obj = Object::new(ctx.clone())?;
            for (key, value) in &provider_config.settings {
                match value {
                    toml::Value::String(value) => settings_obj.set(key.as_str(), value.as_str())?,
                    toml::Value::Integer(value) => settings_obj.set(key.as_str(), *value)?,
                    toml::Value::Float(value) => settings_obj.set(key.as_str(), *value)?,
                    toml::Value::Boolean(value) => settings_obj.set(key.as_str(), *value)?,
                    toml::Value::Array(_) | toml::Value::Table(_) => {
                        let json = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
                        let parsed: Value = ctx.json_parse(json)?;
                        settings_obj.set(key.as_str(), parsed)?;
                    }
                    toml::Value::Datetime(_) => {}
                }
            }
            provider_obj.set("settings", settings_obj)?;
        }
        probe_ctx.set("provider", provider_obj)?;
    }
    probe_ctx.set("app", app)?;
    globals.set("__usagestat_ctx", probe_ctx.clone())?;
    globals.set("__ai_usage_ctx", probe_ctx.clone())?;
    globals.set("__openusage_ctx", probe_ctx.clone())?;
    globals.set("__OPENUSAGE_PLUGIN_REGISTRATION_ID__", manifest.id.as_str())?;
    host_api::inject(ctx, &probe_ctx, &manifest.id)?;
    Ok(())
}

fn parse_metrics(result: &Object<'_>) -> Result<Vec<MetricLine>, String> {
    let lines: Array = result
        .get("metrics")
        .or_else(|_| result.get("lines"))
        .map_err(|_| "missing metrics".to_string())?;
    let mut out = Vec::new();

    for idx in 0..lines.len() {
        let line: Object = lines
            .get(idx)
            .map_err(|_| format!("invalid metric at index {idx}"))?;
        let line_type: String = line.get("type").unwrap_or_default();
        let label: String = line.get("label").unwrap_or_default();
        let color = line.get::<_, String>("color").ok();
        let subtitle = line.get::<_, String>("subtitle").ok();

        match line_type.as_str() {
            "text" => out.push(MetricLine::Text {
                label,
                value: line.get::<_, String>("value").unwrap_or_default(),
                color,
                subtitle,
            }),
            "badge" => out.push(MetricLine::Badge {
                label,
                text: line.get::<_, String>("text").unwrap_or_default(),
                color,
                subtitle,
            }),
            "progress" => out.push(MetricLine::Progress {
                label,
                used: line.get::<_, f64>("used").unwrap_or(0.0),
                limit: line.get::<_, f64>("limit").unwrap_or(100.0),
                format: parse_progress_format(&line),
                resets_at: parse_optional_datetime(line.get::<_, String>("resetsAt").ok()),
                period_duration_ms: line.get::<_, u64>("periodDurationMs").ok(),
                detail: line.get::<_, String>("detail").ok(),
                color,
            }),
            "barChart" => {
                let (chart, errors) = parse_bar_chart_line(&line, idx, label, color);
                for message in errors {
                    out.push(metric_error(message));
                }
                if let Some(chart) = chart {
                    out.push(chart);
                }
            }
            _ => out.push(metric_error(format!(
                "unknown metric type at index {idx}: {line_type}"
            ))),
        }
    }

    Ok(out)
}

const MAX_BAR_CHART_POINTS: usize = 366;

fn parse_bar_chart_line<'js>(
    line: &Object<'js>,
    idx: usize,
    label: String,
    color: Option<String>,
) -> (Option<MetricLine>, Vec<String>) {
    let mut errors = Vec::new();
    let points_array: Array = match line.get("points") {
        Ok(points) => points,
        Err(_) => {
            errors.push(format!("barChart line at index {idx} missing points"));
            return (None, errors);
        }
    };

    let total_points = points_array.len();
    let scan_count = total_points.min(MAX_BAR_CHART_POINTS);
    if total_points > MAX_BAR_CHART_POINTS {
        log::warn!(
            "barChart line at index {idx} has {total_points} points; capping at {MAX_BAR_CHART_POINTS}"
        );
    }

    let mut points = Vec::new();
    for point_idx in 0..scan_count {
        let point: Object = match points_array.get(point_idx) {
            Ok(point) => point,
            Err(_) => {
                errors.push(format!(
                    "barChart line at index {idx} has invalid point at index {point_idx}"
                ));
                continue;
            }
        };

        let point_label = point.get::<_, String>("label").unwrap_or_default();
        let point_label = point_label.trim().to_string();
        if point_label.is_empty() {
            errors.push(format!(
                "barChart line at index {idx} has empty point label at index {point_idx}"
            ));
            continue;
        }

        let value: Value = match point.get("value") {
            Ok(value) => value,
            Err(_) => {
                errors.push(format!(
                    "barChart line at index {idx} point {point_idx} missing value"
                ));
                continue;
            }
        };
        let value = match value.as_number() {
            Some(value) if value.is_finite() && value >= 0.0 => value,
            _ => {
                errors.push(format!(
                    "barChart line at index {idx} point {point_idx} invalid value"
                ));
                continue;
            }
        };

        let value_label = string_value(point.get::<_, Value>("valueLabel").ok());

        points.push(BarChartPoint {
            label: point_label,
            value,
            value_label,
        });
    }

    if points.is_empty() {
        errors.push(format!("barChart line at index {idx} has no valid points"));
        return (None, errors);
    }

    (
        Some(MetricLine::BarChart {
            label,
            points,
            note: string_value(line.get::<_, Value>("note").ok()),
            color,
        }),
        errors,
    )
}

fn string_value(value: Option<Value<'_>>) -> Option<String> {
    let value = value?;
    if value.is_null() || value.is_undefined() {
        return None;
    }
    let value = value.as_string()?;
    let value = value.to_string().ok()?;
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn metric_error(message: impl Into<String>) -> MetricLine {
    MetricLine::Badge {
        label: "Error".to_string(),
        text: message.into(),
        color: Some("red".to_string()),
        subtitle: None,
    }
}

fn parse_progress_format(line: &Object<'_>) -> ProgressFormat {
    let Ok(format) = line.get::<_, Object>("format") else {
        return ProgressFormat::Percent;
    };
    let kind: String = format.get("kind").unwrap_or_else(|_| "percent".to_string());
    match kind.as_str() {
        "dollars" => ProgressFormat::Dollars,
        "count" => ProgressFormat::Count {
            suffix: format.get::<_, String>("suffix").unwrap_or_default(),
        },
        _ => ProgressFormat::Percent,
    }
}

fn parse_optional_datetime(value: Option<String>) -> Option<DateTime<Utc>> {
    value
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

#[cfg(test)]
mod provider_sync_tests {
    use super::*;

    fn fixture(script: &str) -> LoadedProvider {
        LoadedProvider {
            manifest: serde_json::from_value(serde_json::json!({
                "id": "runtime-test", "name": "Runtime Test", "entry": "plugin.js"
            }))
            .unwrap(),
            dir: std::path::PathBuf::from("."),
            entry_script: script.to_string(),
        }
    }

    fn error_text(snapshot: &UsageSnapshot) -> &str {
        assert_eq!(snapshot.source.as_deref(), Some("error"), "{snapshot:?}");
        match &snapshot.metrics[0] {
            MetricLine::Badge { text, .. } => text,
            _ => panic!("expected error badge: {snapshot:?}"),
        }
    }

    #[test]
    fn javascript_errors_include_type_and_plugin_location() {
        for script in [
            "globalThis.__usagestat_plugin = {probe(ctx) { ctx.host.missingFunction(); }};",
            "globalThis.__usagestat_plugin = {async probe(ctx) { await Promise.resolve(); ctx.host.missingFunction(); }};",
            "throw new TypeError('top-level fixture failure');",
            "globalThis.__usagestat_plugin = { probe: function( };",
        ] {
            let snapshot = probe_provider(&fixture(script), "auto", None);
            let message = error_text(&snapshot);
            assert!(message.contains("TypeError:") || message.contains("SyntaxError:"), "{message}");
            assert!(message.contains("runtime-test/plugin.js:1"), "{message}");
        }
        let snapshot = probe_provider(&fixture("globalThis.__usagestat_plugin={probe(){throw 'Your session expired. Sign in again.';}}"), "auto", None);
        assert_eq!(error_text(&snapshot), "Your session expired. Sign in again.");
    }

    #[test]
    fn antigravity_discovery_runs_with_native_host_wrappers() {
        let script = format!(r#"
            __usagestat_ctx.host.ls._discoverReportRaw = function(raw) {{
                if (JSON.parse(raw).processName !== 'language_server') throw new Error('unexpected discovery');
                return JSON.stringify({{status:'ready',result:{{ports:[12345],csrf:'fixture'}}}});
            }};
            __usagestat_ctx.host.http.request = function(req) {{
                if (req.headers['x-codeium-csrf-token'] !== 'fixture') throw new Error('lost CSRF token');
                return {{status:200,bodyText:JSON.stringify({{userStatus:{{userTier:{{name:'Fixture'}},cascadeModelConfigData:{{clientModelConfigs:[
                    {{label:'Gemini Pro',quotaInfo:{{remainingFraction:0.75}}}}
                ]}}}}}})}};
            }};
            {}"#, include_str!("../../../plugins/antigravity/plugin.js"));
        let snapshot = probe_provider(&fixture(&script), "auto", None);
        assert_eq!(snapshot.plan.as_deref(), Some("Fixture"), "{snapshot:?}");
        assert!(matches!(&snapshot.metrics[0], MetricLine::Progress { used, .. } if *used == 25.0));
    }

    #[test]
    fn antigravity_oauth_database_runs_with_native_helpers() {
        let script = format!(r#"
            const ctx = __usagestat_ctx;
            ctx.provider = {{settings:{{ideVariant:'antigravity'}}}};
            const db = ctx.host.fs.appSupportPath('Antigravity/User/globalStorage/state.vscdb');
            if (!db) throw new Error('no app support path');
            ctx.host.fs.exists = path => path === db;
            ctx.host.fs.readText = () => {{throw new Error('unexpected cache read');}};
            ctx.host.ls._discoverReportRaw = () => {{throw new Error('selected profile used discovery');}};
            const field = (n, s) => String.fromCharCode(n * 8 + 2, s.length) + s;
            const token = 'synthetic-access';
            const inner = ctx.base64.encode(field(1, token));
            const outer = ctx.base64.encode(field(1, field(1,'oauthTokenInfoSentinelKey') + field(2, field(1,inner))));
            ctx.host.sqlite.query = path => {{
                if (path !== db) throw new Error('wrong profile');
                return JSON.stringify([{{value:outer}}]);
            }};
            ctx.host.http.request = req => {{
                if (req.headers.Authorization !== 'Bearer ' + token) throw new Error('lost access token');
                return {{status:200,bodyText:JSON.stringify({{models:{{fixture:{{displayName:'Gemini Pro',quotaInfo:{{remainingFraction:0.75}}}}}}}})}};
            }};
            {}"#, include_str!("../../../plugins/antigravity/plugin.js"));
        let snapshot = probe_provider(&fixture(&script), "auto", None);
        assert_ne!(snapshot.source.as_deref(), Some("error"), "{snapshot:?}");
        assert!(matches!(&snapshot.metrics[0], MetricLine::Progress { used, .. } if *used == 25.0));
    }

    #[test]
    fn cached_snapshots_preserve_original_timestamp() {
        let provider = fixture(
            "globalThis.__openusage_plugin = { probe: function(ctx) { return { source: 'cached', fetchedAt: '2026-01-01T00:00:00Z', lines: [ctx.line.progress({label:'Session', used:12, limit:100})] }; } };",
        );
        let snapshot = probe_provider(&provider, "auto", None);
        assert_eq!(snapshot.source.as_deref(), Some("cached"));
        assert_eq!(
            snapshot.fetched_at,
            "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(snapshot.metrics.len(), 1);
    }

    #[test]
    fn structured_provider_settings_survive_native_injection() {
        let config: ProviderConfig = toml::from_str(r#"
            id = "runtime-test"
            [settings]
            sessionRoots = ["/selected/profile/sessions"]
            flags = { partial = true, count = 2 }
        "#).unwrap();
        let provider = fixture("globalThis.__usagestat_plugin = {probe(ctx) { const s=ctx.provider.settings; if (!Array.isArray(s.sessionRoots) || !s.flags.partial || s.flags.count !== 2) throw 'Lost structured settings'; return {lines:[ctx.line.text({label:'Root',value:s.sessionRoots[0]})]}; }};");
        let result = probe_provider(&provider, "auto", Some(&config));
        assert!(matches!(&result.metrics[0], MetricLine::Text { value, .. } if value == "/selected/profile/sessions"), "{result:?}");
    }

    #[test]
    fn asynchronous_bundled_fetchers_run_in_the_native_engine() {
        let script = format!(
            r#"__usagestat_ctx.provider = {{apiKey:'fixture'}};
            __usagestat_ctx.host.http.request = function(req) {{
                if (req.headers.Authorization !== 'Bearer fixture') throw new Error('missing header');
                return {{status:200, headers:{{}}, bodyText:JSON.stringify({{object:'usage',
                  free_tokens:{{used_today:25, limit_per_day:100, remaining:75}}, plan:'Pro'}})}};
            }}; {}"#,
            include_str!("../../../plugins/xkiro/plugin.js")
        );
        let snapshot = probe_provider(&fixture(&script), "api", None);
        assert_eq!(snapshot.state, Some(usagestat_core::model::ProviderState::Ready), "{snapshot:?}");
        assert_eq!(snapshot.plan.as_deref(), Some("Pro"));
        assert!(matches!(&snapshot.metrics[0], MetricLine::Progress { used, .. } if *used == 25.0));
    }

    #[test]
    fn rejected_async_probes_preserve_typed_errors_and_do_not_poison_retries() {
        let provider = fixture("globalThis.__usagestat_plugin = {probe: async function() { await Promise.resolve(); throw {code:'missing-auth',message:'Sign in to the selected account.'}; }};");
        let snapshot = probe_provider(&provider, "auto", None);
        assert_eq!(snapshot.state, Some(usagestat_core::model::ProviderState::MissingAuth));
        let good = fixture("globalThis.__usagestat_plugin = {probe: async function(ctx) { await Promise.resolve(); return {lines:[ctx.line.text({label:'Status',value:'Ready'})]}; }};");
        assert_eq!(probe_provider(&good, "auto", None).state, Some(usagestat_core::model::ProviderState::Ready));
    }

    #[test]
    fn future_and_invalid_cache_dates_use_probe_time() {
        for date in ["invalid", "2999-01-01T00:00:00Z", "1970-01-01T00:00:00Z"] {
            let before = Utc::now();
            let provider = fixture(&format!(
                "globalThis.__openusage_plugin = {{ probe: function(ctx) {{ return {{ fetchedAt: '{date}', lines: [ctx.line.text({{label:'Status',value:'Ready'}})] }}; }} }};"
            ));
            let snapshot = probe_provider(&provider, "auto", None);
            assert!(snapshot.fetched_at >= before && snapshot.fetched_at <= Utc::now());
        }
    }

    #[test]
    fn crypto_and_url_validation_are_available_in_quickjs() {
        let provider = fixture(
            "globalThis.__openusage_plugin = { probe: function(ctx) { return { lines: [ctx.line.text({label:'Fingerprint',value:ctx.host.crypto.sha256('abc')}), ctx.line.text({label:'URL',value:ctx.host.http.validateBaseUrl('http://[::1]:8080/v1/',true)})] }; } };",
        );
        let snapshot = probe_provider(&provider, "auto", None);
        assert_ne!(snapshot.source.as_deref(), Some("error"));
        assert!(
            matches!(&snapshot.metrics[0], MetricLine::Text { value, .. } if value == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert!(
            matches!(&snapshot.metrics[1], MetricLine::Text { value, .. } if value == "http://[::1]:8080/v1")
        );
    }
}
