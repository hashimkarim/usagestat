use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;
use usagestat_core::{paths, process};

const CCUSAGE_VERSION: &str = "20.0.2";
const CCUSAGE_PACKAGE_NAME: &str = "ccusage";
const CCUSAGE_BIN_NAME: &str = "ccusage";
const CCUSAGE_LEGACY_VERSION: &str = "18.0.11";
const CCUSAGE_LEGACY_CLAUDE_PACKAGE_NAME: &str = "ccusage";
const CCUSAGE_LEGACY_CODEX_PACKAGE_NAME: &str = "@ccusage/codex";
const CCUSAGE_LEGACY_CODEX_BIN_NAME: &str = "ccusage-codex";
const CCUSAGE_TIMEOUT_SECS: u64 = 30;
const CCUSAGE_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcusageQueryOpts {
    pub provider: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub home_path: Option<String>,
    pub claude_path: Option<String>,
    pub time_zone: Option<String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CcusageProvider {
    Claude,
    Codex,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CcusageRunnerKind {
    Bunx,
    PnpmDlx,
    YarnDlx,
    NpmExec,
    Npx,
}

#[derive(Debug, Eq, PartialEq)]
enum CcusageRunnerResult {
    Success(String),
    Failed,
    TimedOut,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CcusageCommandFlavor {
    Current,
    Legacy,
}

#[derive(Copy, Clone)]
struct CcusageProviderConfig {
    command_namespace: &'static str,
    home_env_var: &'static str,
}

pub fn parse_provider(value: &str) -> Option<CcusageProvider> {
    match value.trim().to_ascii_lowercase().as_str() {
        "claude" | "anthropic" => Some(CcusageProvider::Claude),
        "codex" | "openai" | "chatgpt" => Some(CcusageProvider::Codex),
        _ => None,
    }
}

pub fn provider_id(provider: CcusageProvider) -> &'static str {
    match provider {
        CcusageProvider::Claude => "claude",
        CcusageProvider::Codex => "codex",
    }
}

pub fn resolve_provider(opts: &CcusageQueryOpts, plugin_id: &str) -> CcusageProvider {
    opts.provider
        .as_deref()
        .and_then(parse_provider)
        .or_else(|| parse_provider(plugin_id))
        .unwrap_or(CcusageProvider::Claude)
}

pub fn query_status_json(opts: &CcusageQueryOpts, plugin_id: &str) -> String {
    let provider = resolve_provider(opts, plugin_id);
    // Pass the zone explicitly and stamp that exact zone on the result. Inferring
    // it later at store-read time would mislabel historical or imported rows.
    let mut opts = opts.clone();
    let time_zone = opts.time_zone.clone().filter(|zone| !zone.trim().is_empty())
        .or_else(|| std::env::var("TZ").ok().filter(|zone| !zone.trim().is_empty()))
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_else(|| "UTC".to_string());
    opts.time_zone = Some(time_zone.trim().trim_start_matches(':').to_string());
    let runners = collect_runners();
    if runners.is_empty() {
        return serde_json::json!({ "status": "no_runner" }).to_string();
    }

    for (kind, program) in runners {
        match run_with_runner(kind, &program, &opts, provider) {
            CcusageRunnerResult::Success(result) => {
                let Ok(mut data) = serde_json::from_str::<JsonValue>(&result) else {
                    continue;
                };
                stamp_time_zone(&mut data, opts.time_zone.as_deref().unwrap());
                return serde_json::json!({ "status": "ok", "data": data }).to_string();
            }
            CcusageRunnerResult::Failed => {}
            CcusageRunnerResult::TimedOut => {
                return serde_json::json!({ "status": "runner_failed" }).to_string();
            }
        }
    }

    serde_json::json!({ "status": "runner_failed" }).to_string()
}

fn stamp_time_zone(data: &mut JsonValue, time_zone: &str) {
    if let Some(daily) = data.get_mut("daily").and_then(JsonValue::as_array_mut) {
        for row in daily {
            if let Some(row) = row.as_object_mut() {
                row.insert("timeZone".into(), JsonValue::String(time_zone.into()));
            }
        }
    }
    if let Some(data) = data.as_object_mut() {
        data.insert("timeZone".into(), JsonValue::String(time_zone.into()));
    }
}

pub fn query_daily(opts: &CcusageQueryOpts, plugin_id: &str) -> Result<JsonValue, String> {
    let status_json = query_status_json(opts, plugin_id);
    let status: JsonValue = serde_json::from_str(&status_json).map_err(|e| e.to_string())?;
    match status.get("status").and_then(|v| v.as_str()) {
        Some("ok") => status
            .get("data")
            .cloned()
            .ok_or_else(|| "missing ccusage data".to_string()),
        Some(other) => Err(other.to_string()),
        None => Err("invalid ccusage response".to_string()),
    }
}

fn provider_config(provider: CcusageProvider) -> CcusageProviderConfig {
    match provider {
        CcusageProvider::Claude => CcusageProviderConfig {
            command_namespace: "claude",
            home_env_var: "CLAUDE_CONFIG_DIR",
        },
        CcusageProvider::Codex => CcusageProviderConfig {
            command_namespace: "codex",
            home_env_var: "CODEX_HOME",
        },
    }
}

fn supports_legacy_fallback(provider: CcusageProvider) -> bool {
    matches!(provider, CcusageProvider::Claude | CcusageProvider::Codex)
}

fn package_spec() -> String {
    format!("{}@{}", CCUSAGE_PACKAGE_NAME, CCUSAGE_VERSION)
}

fn legacy_package_spec(provider: CcusageProvider) -> String {
    let package_name = match provider {
        CcusageProvider::Claude => CCUSAGE_LEGACY_CLAUDE_PACKAGE_NAME,
        CcusageProvider::Codex => CCUSAGE_LEGACY_CODEX_PACKAGE_NAME,
    };
    format!("{package_name}@{CCUSAGE_LEGACY_VERSION}")
}

fn runner_order() -> [CcusageRunnerKind; 5] {
    [
        CcusageRunnerKind::Bunx,
        CcusageRunnerKind::PnpmDlx,
        CcusageRunnerKind::YarnDlx,
        CcusageRunnerKind::NpmExec,
        CcusageRunnerKind::Npx,
    ]
}

fn runner_candidates(kind: CcusageRunnerKind) -> Vec<String> {
    if std::env::var_os("USAGESTAT_HELPER_PATH").is_some_and(|value| !value.is_empty()) {
        return vec![runner_name(kind).into()];
    }
    #[cfg(windows)]
    return vec![runner_name(kind).into()];
    #[cfg(not(windows))]
    {
        let mut candidates = Vec::new();
        match kind {
            CcusageRunnerKind::Bunx => {
                if let Some(home) = paths::home_dir() {
                    candidates.push(home.join(".bun/bin/bunx").to_string_lossy().to_string());
                }
                candidates.extend(
                    ["/opt/homebrew/bin/bunx", "/usr/local/bin/bunx", "bunx"].map(String::from),
                );
            }
            CcusageRunnerKind::PnpmDlx => {
                candidates.extend(
                    ["/opt/homebrew/bin/pnpm", "/usr/local/bin/pnpm", "pnpm"].map(String::from),
                );
            }
            CcusageRunnerKind::YarnDlx => {
                candidates.extend(
                    ["/opt/homebrew/bin/yarn", "/usr/local/bin/yarn", "yarn"].map(String::from),
                );
            }
            CcusageRunnerKind::NpmExec => {
                candidates.extend(
                    ["/opt/homebrew/bin/npm", "/usr/local/bin/npm", "npm"].map(String::from),
                );
            }
            CcusageRunnerKind::Npx => {
                candidates.extend(
                    ["/opt/homebrew/bin/npx", "/usr/local/bin/npx", "npx"].map(String::from),
                );
            }
        }

        let mut unique = Vec::new();
        for candidate in candidates {
            if !candidate.is_empty() && !unique.iter().any(|seen| seen == &candidate) {
                unique.push(candidate);
            }
        }
        unique
    }
}

fn runner_name(kind: CcusageRunnerKind) -> &'static str {
    match kind {
        CcusageRunnerKind::Bunx => "bunx",
        CcusageRunnerKind::PnpmDlx => "pnpm",
        CcusageRunnerKind::YarnDlx => "yarn",
        CcusageRunnerKind::NpmExec => "npm",
        CcusageRunnerKind::Npx => "npx",
    }
}

fn path_entries_with(home: Option<&Path>, existing_path: Option<&OsStr>) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut entries: Vec<PathBuf> = existing_path
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .collect();
        for key in ["PNPM_HOME", "NVM_SYMLINK"] {
            if let Some(value) = std::env::var_os(key).filter(|value| !value.is_empty()) {
                entries.push(PathBuf::from(value));
            }
        }
        if let Some(roaming) = dirs::data_dir() {
            entries.push(roaming.join("npm"));
        }
        if let Some(home) = home {
            entries.push(home.join(".bun/bin"));
        }
        return entries;
    }
    #[cfg(not(windows))]
    {
        let mut entries = Vec::new();
        if let Some(home) = home {
            entries.push(home.join(".bun/bin"));
            entries.push(home.join(".nvm/current/bin"));
            entries.extend(nvm_node_bin_paths(home));
            entries.push(home.join(".local/bin"));
        }
        entries.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
        if let Some(existing_path) = existing_path {
            entries.extend(std::env::split_paths(existing_path));
        }

        let mut unique = Vec::new();
        for entry in entries {
            if !entry.as_os_str().is_empty() && !unique.iter().any(|seen| seen == &entry) {
                unique.push(entry);
            }
        }
        unique
    }
}

#[cfg(not(windows))]
fn nvm_node_bin_paths(home: &Path) -> Vec<PathBuf> {
    let nvm_dir = home.join(".nvm");
    let Some(version) = resolve_nvm_alias(&nvm_dir, "default", 0) else {
        return Vec::new();
    };
    vec![nvm_dir.join("versions/node").join(version).join("bin")]
}

#[cfg(not(windows))]
fn resolve_nvm_alias(nvm_dir: &Path, alias: &str, depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }
    let raw = std::fs::read_to_string(nvm_dir.join("alias").join(alias)).ok()?;
    let value = raw
        .lines()
        .next()
        .unwrap_or_default()
        .split('#')
        .next()
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with('v') {
        return Some(value.to_string());
    }
    resolve_nvm_alias(nvm_dir, value, depth + 1)
}

fn enriched_path() -> Option<OsString> {
    if let Some(path) = std::env::var_os("USAGESTAT_HELPER_PATH").filter(|path| !path.is_empty()) {
        return Some(path);
    }
    let home = paths::home_dir();
    let existing_path = std::env::var_os("PATH");
    std::env::join_paths(path_entries_with(home.as_deref(), existing_path.as_deref())).ok()
}

fn runner_available(candidate: &str, enriched_path: Option<&OsStr>) -> bool {
    let Ok(mut command) = process::command_with_path(OsStr::new(candidate), enriched_path) else {
        return false;
    };
    command.arg("--version");
    process::run(command, Duration::from_secs(5), 4096).is_ok_and(|output| output.status.success())
}

fn collect_runners() -> Vec<(CcusageRunnerKind, String)> {
    let path = enriched_path();
    let mut runners = Vec::new();
    for kind in runner_order() {
        for candidate in runner_candidates(kind) {
            if runner_available(&candidate, path.as_deref()) {
                runners.push((kind, candidate));
                break;
            }
        }
    }
    runners
}

fn append_common_args(
    args: &mut Vec<String>,
    opts: &CcusageQueryOpts,
    provider: CcusageProvider,
    flavor: CcusageCommandFlavor,
) {
    if flavor == CcusageCommandFlavor::Current {
        args.push(provider_config(provider).command_namespace.to_string());
    }
    args.extend([
        "daily".to_string(),
        "--json".to_string(),
        "--order".to_string(),
        "desc".to_string(),
    ]);

    if let Some(time_zone) = &opts.time_zone {
        args.extend(["--timezone".into(), time_zone.clone()]);
    }

    if let Some(since) = opts
        .since
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        args.push("--since".to_string());
        args.push(since.to_string());
    }
    if let Some(until) = opts
        .until
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        args.push("--until".to_string());
        args.push(until.to_string());
    }
}

fn runner_args(
    kind: CcusageRunnerKind,
    opts: &CcusageQueryOpts,
    provider: CcusageProvider,
    flavor: CcusageCommandFlavor,
) -> Vec<String> {
    let package = match flavor {
        CcusageCommandFlavor::Current => package_spec(),
        CcusageCommandFlavor::Legacy => legacy_package_spec(provider),
    };
    let npm_exec_bin = match (flavor, provider) {
        (CcusageCommandFlavor::Current, _)
        | (CcusageCommandFlavor::Legacy, CcusageProvider::Claude) => CCUSAGE_BIN_NAME,
        (CcusageCommandFlavor::Legacy, CcusageProvider::Codex) => CCUSAGE_LEGACY_CODEX_BIN_NAME,
    };
    let mut args = match kind {
        CcusageRunnerKind::Bunx => vec!["--silent".to_string(), package],
        CcusageRunnerKind::PnpmDlx => vec!["-s".to_string(), "dlx".to_string(), package],
        CcusageRunnerKind::YarnDlx => vec!["dlx".to_string(), "-q".to_string(), package],
        CcusageRunnerKind::NpmExec => vec![
            "exec".to_string(),
            "--yes".to_string(),
            format!("--package={package}"),
            "--".to_string(),
            npm_exec_bin.to_string(),
        ],
        CcusageRunnerKind::Npx => vec!["--yes".to_string(), package],
    };
    append_common_args(&mut args, opts, provider, flavor);
    args
}

fn home_override(opts: &CcusageQueryOpts) -> Option<&str> {
    opts.home_path
        .as_deref()
        .or(opts.claude_path.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn run_with_runner(
    kind: CcusageRunnerKind,
    program: &str,
    opts: &CcusageQueryOpts,
    provider: CcusageProvider,
) -> CcusageRunnerResult {
    let current =
        run_with_runner_flavor(kind, program, opts, provider, CcusageCommandFlavor::Current);
    match current {
        CcusageRunnerResult::Failed if supports_legacy_fallback(provider) => {
            run_with_runner_flavor(kind, program, opts, provider, CcusageCommandFlavor::Legacy)
        }
        other => other,
    }
}

fn run_with_runner_flavor(
    kind: CcusageRunnerKind,
    program: &str,
    opts: &CcusageQueryOpts,
    provider: CcusageProvider,
    flavor: CcusageCommandFlavor,
) -> CcusageRunnerResult {
    let args = runner_args(kind, opts, provider, flavor);
    let path = enriched_path();
    let Ok(mut command) = process::command_with_path(OsStr::new(program), path.as_deref()) else {
        return CcusageRunnerResult::Failed;
    };
    command.args(&args);
    if let Some(home_path) = home_override(opts) {
        command.env(
            provider_config(provider).home_env_var,
            paths::expand_home(home_path),
        );
    }
    match process::run(
        command,
        Duration::from_secs(CCUSAGE_TIMEOUT_SECS),
        CCUSAGE_OUTPUT_LIMIT_BYTES,
    ) {
        Ok(output) if output.status.success() => {
            normalize_output(&process::decode_output(&output.stdout))
                .map(CcusageRunnerResult::Success)
                .unwrap_or(CcusageRunnerResult::Failed)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
            ) =>
        {
            CcusageRunnerResult::TimedOut
        }
        _ => CcusageRunnerResult::Failed,
    }
}

fn extract_last_json_value(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if serde_json::from_str::<JsonValue>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }
    let mut starts: Vec<usize> = trimmed
        .char_indices()
        .filter(|(_, c)| *c == '{' || *c == '[')
        .map(|(idx, _)| idx)
        .collect();
    starts.reverse();
    for start in starts {
        let candidate = trimmed[start..].trim();
        if serde_json::from_str::<JsonValue>(candidate).is_ok() {
            return Some(candidate.to_string());
        }
    }
    None
}

fn normalize_output(stdout: &str) -> Option<String> {
    let json_value = extract_last_json_value(stdout)?;
    let parsed: JsonValue = serde_json::from_str(&json_value).ok()?;
    let normalized = match parsed {
        JsonValue::Array(daily) => serde_json::json!({ "daily": daily }),
        JsonValue::Object(map) => {
            let daily = map.get("daily")?;
            if !daily.is_array() {
                return None;
            }
            JsonValue::Object(map)
        }
        _ => return None,
    };
    serde_json::to_string(&normalized).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_runner_args_use_unified_package_with_provider_namespace() {
        let opts = CcusageQueryOpts {
            since: Some("20260101".to_string()),
            ..Default::default()
        };

        assert_eq!(
            runner_args(
                CcusageRunnerKind::Bunx,
                &opts,
                CcusageProvider::Claude,
                CcusageCommandFlavor::Current
            ),
            vec![
                "--silent",
                "ccusage@20.0.2",
                "claude",
                "daily",
                "--json",
                "--order",
                "desc",
                "--since",
                "20260101",
            ]
        );

        assert_eq!(
            runner_args(
                CcusageRunnerKind::NpmExec,
                &opts,
                CcusageProvider::Codex,
                CcusageCommandFlavor::Current
            ),
            vec![
                "exec",
                "--yes",
                "--package=ccusage@20.0.2",
                "--",
                "ccusage",
                "codex",
                "daily",
                "--json",
                "--order",
                "desc",
                "--since",
                "20260101",
            ]
        );
    }

    #[test]
    fn legacy_runner_args_preserve_old_package_shapes() {
        let opts = CcusageQueryOpts::default();

        assert_eq!(
            runner_args(
                CcusageRunnerKind::Bunx,
                &opts,
                CcusageProvider::Claude,
                CcusageCommandFlavor::Legacy
            ),
            vec![
                "--silent",
                "ccusage@18.0.11",
                "daily",
                "--json",
                "--order",
                "desc"
            ]
        );

        assert_eq!(
            runner_args(
                CcusageRunnerKind::NpmExec,
                &opts,
                CcusageProvider::Codex,
                CcusageCommandFlavor::Legacy
            ),
            vec![
                "exec",
                "--yes",
                "--package=@ccusage/codex@18.0.11",
                "--",
                "ccusage-codex",
                "daily",
                "--json",
                "--order",
                "desc",
            ]
        );
    }

    #[test]
    fn provenance_uses_the_explicit_zone_for_current_and_legacy_reports() {
        let opts = CcusageQueryOpts { time_zone: Some("Europe/Amsterdam".into()), ..Default::default() };
        for provider in [CcusageProvider::Claude, CcusageProvider::Codex] {
            for flavor in [CcusageCommandFlavor::Current, CcusageCommandFlavor::Legacy] {
                let args = runner_args(CcusageRunnerKind::Bunx, &opts, provider, flavor);
                assert!(args.windows(2).any(|args| args == ["--timezone", "Europe/Amsterdam"]));
            }
        }
        let mut data = serde_json::json!({"daily":[{"date":"2026-09-27","totalTokens":10}]});
        stamp_time_zone(&mut data, opts.time_zone.as_deref().unwrap());
        assert_eq!(data["timeZone"], "Europe/Amsterdam");
        assert_eq!(data["daily"][0]["timeZone"], "Europe/Amsterdam");
    }

    #[test]
    fn capped_output_decode_drops_incomplete_trailing_utf8() {
        let mut bytes = "usage ".as_bytes().to_vec();
        bytes.extend_from_slice(&[0xE2, 0x82]);

        assert_eq!(process::decode_output(&bytes), "usage ");
    }
}
