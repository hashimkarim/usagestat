//! Read quota observations owned by the selected Claude profile. Never infer
//! allowance from token logs, launch a prompt, or persist a conversation.
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use usagestat_core::{paths, process, storage};

const MAX_FILE: u64 = 4 * 1024 * 1024;
const MAX_LINE: usize = 256 * 1024;
const FRESH_MS: i64 = 5 * 60 * 1000;
const WINDOWS: &[&str] = &[
    "five_hour",
    "seven_day",
    "seven_day_sonnet",
    "seven_day_opus",
    "seven_day_routines",
    "seven_day_cowork",
    "seven_day_claude_routines",
];

fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

fn read_json(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FILE)
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn auth_overridden() -> bool {
    [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_CUSTOM_OAUTH_URL",
        "CLAUDE_CODE_OAUTH_CLIENT_ID",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "USE_LOCAL_OAUTH",
        "USE_STAGING_OAUTH",
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

// No Debug: Claude's configuration and credentials may contain secrets.
struct Profile {
    config_path: PathBuf,
    credentials_path: PathBuf,
    identity: String,
    token_hash: String,
    account_uuid: String,
    email: String,
    organization: String,
    organization_name: Option<String>,
    config: Value,
}

impl Profile {
    fn load() -> Option<Self> {
        if auth_overridden() {
            return None;
        }
        let home = dirs::home_dir()?;
        let explicit = env_path("CLAUDE_CONFIG_DIR");
        let root = explicit.clone().unwrap_or_else(|| home.join(".claude"));
        let config = if root.join(".config.json").is_file() {
            root.join(".config.json")
        } else if explicit.is_some() {
            root.join(".claude.json")
        } else {
            home.join(".claude.json")
        };
        Self::at(config, root.join(".credentials.json"))
    }

    fn at(config_path: PathBuf, credentials_path: PathBuf) -> Option<Self> {
        let config = read_json(&config_path)?;
        let account = config.get("oauthAccount")?;
        let account_uuid = text(account, "accountUuid")?;
        let email = text(account, "emailAddress")?;
        let organization = text(account, "organizationUuid")?;
        let organization_name = text(account, "organizationName");
        // Fail closed when credential ownership cannot be verified (including
        // keychain-only profiles); the existing OAuth path still handles those.
        let credentials = read_json(&credentials_path)?;
        let token = credentials
            .get("claudeAiOauth")?
            .get("accessToken")?
            .as_str()?;
        if token.is_empty() {
            return None;
        }
        let token_hash = hash(token);
        let identity = hash(
            serde_json::to_vec(&json!([
                config_path.canonicalize().ok()?,
                credentials_path.canonicalize().ok()?,
                account_uuid,
                organization,
                token_hash
            ]))
            .ok()?,
        );
        Some(Self {
            config_path,
            credentials_path,
            identity,
            token_hash,
            account_uuid,
            email,
            organization,
            organization_name,
            config,
        })
    }

    fn unchanged(&self) -> bool {
        Self::at(self.config_path.clone(), self.credentials_path.clone())
            .is_some_and(|profile| profile.identity == self.identity)
    }

    fn matches_account(&self, account: &Value) -> bool {
        account
            .get("email")
            .and_then(Value::as_str)
            .is_some_and(|email| email.eq_ignore_ascii_case(&self.email))
            && account
                .get("organization")
                .and_then(Value::as_str)
                .is_some_and(|org| {
                    org == self.organization || self.organization_name.as_deref() == Some(org)
                })
            && account.get("apiProvider").and_then(Value::as_str) == Some("firstParty")
    }

    fn cache_path(&self) -> Option<PathBuf> {
        Some(
            paths::data_dir()
                .ok()?
                .join("plugins/claude/cli-usage")
                .join(format!("{}.json", self.identity)),
        )
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Window {
    utilization: f64,
    resets_at: Option<String>,
}

impl Window {
    fn parse(value: &Value, now: i64) -> Option<Self> {
        let utilization = value.get("utilization")?.as_f64()?;
        if !utilization.is_finite() || !(0.0..=100.0).contains(&utilization) {
            return None;
        }
        let resets_at = match value.get("resets_at") {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                let reset = DateTime::parse_from_rfc3339(raw).ok()?;
                if reset.timestamp_millis() <= now {
                    return None;
                }
                Some(reset.with_timezone(&Utc).to_rfc3339())
            }
            _ => return None,
        };
        Some(Self {
            utilization,
            resets_at,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    window: Window,
    fetched_at_ms: i64,
    source: String,
}

type Observations = BTreeMap<String, Observation>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Reading {
    data: BTreeMap<String, Window>,
    fetched_at_ms: i64,
    source: String,
}

fn fresh(at: i64, now: i64) -> bool {
    at > 0 && at <= now && now - at <= FRESH_MS
}

fn usage_observations(data: &Value, at: i64, source: &str, now: i64) -> Observations {
    if !fresh(at, now) {
        return Observations::new();
    }
    WINDOWS
        .iter()
        .filter_map(|name| {
            let kind = match *name {
                "five_hour" => Some("session"),
                "seven_day" => Some("weekly_all"),
                _ => None,
            };
            let current = kind.and_then(|kind| {
                let entries: Vec<_> = data
                    .get("limits")?
                    .as_array()?
                    .iter()
                    .filter(|entry| {
                        entry["kind"] == kind
                            && entry["percent"].is_number()
                            && entry["scope"]["model"].is_null()
                            && entry["scope"]["surface"].is_null()
                    })
                    .collect();
                let entry = entries
                    .iter()
                    .find(|entry| entry["is_active"] == true)
                    .or(entries.first())?;
                Some(json!({"utilization": entry["percent"], "resets_at": entry["resets_at"]}))
            });
            Some((
                (*name).into(),
                Observation {
                    window: Window::parse(current.as_ref().or_else(|| data.get(*name))?, now)?,
                    fetched_at_ms: at,
                    source: source.into(),
                },
            ))
        })
        .collect()
}

fn cached_observations(profile: &Profile, now: i64) -> Observations {
    let Some(cache) = profile.config.get("cachedUsageUtilization") else {
        return Observations::new();
    };
    if cache.get("accountUuid").and_then(Value::as_str) != Some(profile.account_uuid.as_str()) {
        return Observations::new();
    }
    usage_observations(
        &cache["utilization"],
        cache["fetchedAtMs"].as_i64().unwrap_or(0),
        "cli-cache",
        now,
    )
}

fn load_observations(path: &Path, identity: &str, now: i64) -> Observations {
    let Some(stored) = read_json(path) else {
        return Observations::new();
    };
    if stored["identity"].as_str() != Some(identity) || stored["version"] != 1 {
        return Observations::new();
    }
    serde_json::from_value::<Observations>(stored["windows"].clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, item)| {
            WINDOWS.contains(&name.as_str())
                && fresh(item.fetched_at_ms, now)
                && item.source == "cli"
                && Window::parse(
                    &serde_json::to_value(&item.window).unwrap_or(Value::Null),
                    now,
                )
                .is_some()
        })
        .collect()
}

fn merge(windows: &mut Observations, updates: Observations) {
    for (name, update) in updates {
        if windows
            .get(&name)
            .is_none_or(|old| old.fetched_at_ms < update.fetched_at_ms)
        {
            windows.insert(name, update);
        }
    }
}

fn reading(windows: Observations) -> Option<Reading> {
    // A sparse response must not make an older window look newly fetched.
    let fetched_at_ms = windows.values().map(|item| item.fetched_at_ms).min()?;
    let source = if windows.values().any(|item| item.source == "cli") {
        "cli"
    } else {
        "cli-cache"
    };
    Some(Reading {
        data: windows
            .into_iter()
            .map(|(name, item)| (name, item.window))
            .collect(),
        fetched_at_ms,
        source: source.into(),
    })
}

fn profile_reading(profile: &Profile, now: i64) -> Option<Reading> {
    let mut windows = cached_observations(profile, now);
    if let Some(path) = profile.cache_path() {
        merge(
            &mut windows,
            load_observations(&path, &profile.identity, now),
        );
    }
    profile.unchanged().then(|| reading(windows)).flatten()
}

pub(crate) fn read(expected_token_hash: &str, organization: Option<&str>) -> Option<Reading> {
    let profile = Profile::load()?;
    if profile.token_hash != expected_token_hash
        || organization
            .filter(|value| !value.is_empty())
            .is_some_and(|org| org != profile.organization)
    {
        return None;
    }
    profile_reading(&profile, Utc::now().timestamp_millis())
}

fn store_observations(profile: &Profile, updates: Observations, now: i64) -> io::Result<()> {
    if updates.is_empty() || !profile.unchanged() {
        return Ok(());
    }
    let Some(path) = profile.cache_path() else {
        return Ok(());
    };
    storage::private_directory(path.parent().expect("cache directory"))?;
    let _lock = storage::exclusive_lock(&path.with_extension("lock"))?;
    let mut windows = load_observations(&path, &profile.identity, now);
    merge(&mut windows, updates);
    if !profile.unchanged() {
        return Ok(());
    }
    storage::write_atomic(
        &path,
        &serde_json::to_vec(
            &json!({ "version": 1, "identity": profile.identity, "windows": windows }),
        )?,
    )
}

#[derive(Default)]
struct JsonLines {
    line: Vec<u8>,
    oversized: bool,
}

impl JsonLines {
    fn push(&mut self, bytes: &[u8], mut receive: impl FnMut(Value)) {
        for &byte in bytes {
            if byte == b'\n' {
                if !self.oversized
                    && let Ok(value) = serde_json::from_slice(&self.line)
                {
                    receive(value);
                }
                self.line.clear();
                self.oversized = false;
            } else if !self.oversized {
                if self.line.len() == MAX_LINE {
                    self.line.clear();
                    self.oversized = true;
                } else {
                    self.line.push(byte);
                }
            }
        }
    }
}

fn control(id: &str, subtype: &str) -> Vec<u8> {
    let request = if subtype == "initialize" {
        json!({"subtype": subtype, "hooks": {}})
    } else {
        json!({"subtype": subtype, "skip_behaviors": true})
    };
    let mut line =
        serde_json::to_vec(&json!({"type":"control_request", "request_id": id, "request":request}))
            .expect("static protocol request");
    line.push(b'\n');
    line
}

#[derive(Default)]
struct Protocol {
    initialized: bool,
    finished: bool,
    usage: Option<Value>,
}

impl Protocol {
    fn accept(&mut self, message: &Value, profile: &Profile) -> process::StreamControl {
        if self.finished {
            return process::StreamControl::Finish;
        }
        if message["type"] != "control_response" {
            return process::StreamControl::Continue;
        }
        let response = &message["response"];
        match response["request_id"].as_str() {
            Some("usagestat-init") if !self.initialized => {
                self.initialized = response["subtype"] == "success"
                    && profile.matches_account(&response["response"]["account"]);
                if self.initialized {
                    process::StreamControl::Reply(control("usagestat-quota", "get_usage"))
                } else {
                    self.finished = true;
                    process::StreamControl::Finish
                }
            }
            Some("usagestat-quota") if self.initialized => {
                if response["subtype"] == "success"
                    && response["response"]["rate_limits_available"] == true
                {
                    self.usage = response["response"].get("rate_limits").cloned();
                }
                self.finished = true;
                process::StreamControl::Finish
            }
            _ => process::StreamControl::Continue,
        }
    }
}

fn run_probe(mut command: std::process::Command, profile: &Profile) -> io::Result<Option<Reading>> {
    command
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--tools",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--setting-sources=",
            "--settings",
            "{\"disableAllHooks\":true}",
            "--disable-slash-commands",
        ])
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    // A neutral cwd and disabled hooks/MCP prevent a usage check from running
    // repository tools or custom startup actions.
    let cwd = storage::temporary_directory()?;
    command.current_dir(cwd.path());
    let mut parser = JsonLines::default();
    let mut protocol = Protocol::default();
    process::control_dialogue(
        command,
        &control("usagestat-init", "initialize"),
        Duration::from_secs(12),
        1024 * 1024,
        |bytes| {
            let mut action = process::StreamControl::Continue;
            parser.push(bytes, |message| {
                let next = protocol.accept(&message, profile);
                if !matches!(next, process::StreamControl::Continue) {
                    action = next;
                }
            });
            Ok(action)
        },
    )?;
    if !profile.unchanged() {
        return Ok(None);
    }
    let Some(usage) = protocol.usage else {
        return Ok(None);
    };
    let now = Utc::now().timestamp_millis();
    // Claude may answer get_usage from its own cache. Keep that observation's
    // time instead of turning an old response into a fresh reading.
    let Some(latest) = Profile::at(
        profile.config_path.clone(),
        profile.credentials_path.clone(),
    ) else {
        return Ok(None);
    };
    if latest.identity != profile.identity {
        return Ok(None);
    }
    let Some(at) = observation_time(&latest.config, &profile.account_uuid, now) else {
        return Ok(None);
    };
    let observations = usage_observations(&usage, at, "cli", now);
    store_observations(profile, observations.clone(), now)?;
    Ok(reading(observations))
}

fn observation_time(config: &Value, account: &str, now: i64) -> Option<i64> {
    match config.get("cachedUsageUtilization") {
        // Older CLIs do not persist this cache; their control response is the
        // only observation. Newer CLIs persist successful endpoint readings.
        None => Some(now),
        Some(cache) if cache["accountUuid"].as_str() == Some(account) => {
            cache["fetchedAtMs"].as_i64()
        }
        _ => None,
    }
}

/// Fixed, prompt-free protocol only. No arbitrary command or prompt is exposed
/// to plugins. Callers still must respect the OAuth Retry-After deadline.
pub(crate) fn probe(
    expected_token_hash: &str,
    organization: Option<&str>,
) -> io::Result<Option<Reading>> {
    let Some(profile) = Profile::load() else {
        return Ok(None);
    };
    if profile.token_hash != expected_token_hash
        || organization
            .filter(|value| !value.is_empty())
            .is_some_and(|org| org != profile.organization)
    {
        return Ok(None);
    }
    let Some(path) = profile.cache_path() else {
        return Ok(None);
    };
    storage::private_directory(path.parent().expect("cache directory"))?;
    let attempt_path = path.with_extension("attempt.json");
    let _lock = storage::exclusive_lock(&path.with_extension("probe.lock"))?;
    let now = Utc::now().timestamp_millis();
    if read_json(&attempt_path)
        .and_then(|value| value["at"].as_i64())
        .is_some_and(|at| fresh(at, now))
    {
        return Ok(profile_reading(&profile, now));
    }
    storage::write_atomic(&attempt_path, &serde_json::to_vec(&json!({"at": now}))?)?;
    run_probe(process::command("claude")?, &profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1_790_000_000_000;

    fn fixture() -> (tempfile::TempDir, Profile) {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join(".claude.json");
        let credentials_path = dir.path().join(".credentials.json");
        std::fs::write(&config_path, serde_json::to_vec(&json!({
            "oauthAccount": {"accountUuid":"account-a", "emailAddress":"a@example.test", "organizationUuid":"org-a", "organizationName":"Test organization"},
            "cachedUsageUtilization": {"accountUuid":"account-a", "fetchedAtMs": NOW - 1000, "utilization": {
                "five_hour":{"utilization":0,"resets_at":null},
                "seven_day":{"utilization":8,"resets_at":"2030-01-01T00:00:00Z"}
            }}
        })).unwrap()).unwrap();
        std::fs::write(
            &credentials_path,
            r#"{"claudeAiOauth":{"accessToken":"fake-test-token"}}"#,
        )
        .unwrap();
        let profile = Profile::at(config_path, credentials_path).unwrap();
        (dir, profile)
    }

    #[test]
    fn claude_owned_cache_preserves_zero_and_observation_time() {
        let (_dir, profile) = fixture();
        let result = reading(cached_observations(&profile, NOW)).unwrap();
        assert_eq!(result.fetched_at_ms, NOW - 1000);
        assert_eq!(result.source, "cli-cache");
        assert_eq!(result.data["five_hour"].utilization, 0.0);
        assert_eq!(result.data["seven_day"].utilization, 8.0);
    }

    #[test]
    fn cache_rejects_another_account_stale_and_future_readings() {
        let (_dir, mut profile) = fixture();
        for at in [0, NOW + 1, NOW - FRESH_MS - 1] {
            profile.config["cachedUsageUtilization"]["fetchedAtMs"] = json!(at);
            assert!(cached_observations(&profile, NOW).is_empty());
        }
        profile.config["cachedUsageUtilization"]["fetchedAtMs"] = json!(NOW);
        profile.config["cachedUsageUtilization"]["accountUuid"] = json!("account-b");
        assert!(cached_observations(&profile, NOW).is_empty());
    }

    #[test]
    fn absent_invalid_and_expired_windows_are_not_zero() {
        for data in [
            Value::Null,
            json!({}),
            json!({"utilization":null}),
            json!({"utilization":-1}),
            json!({"utilization":101}),
            json!({"utilization":"8"}),
            json!({"utilization":5,"resets_at":"invalid"}),
            json!({"utilization":5,"resets_at":"2020-01-01T00:00:00Z"}),
        ] {
            assert!(Window::parse(&data, NOW).is_none());
        }
    }

    #[test]
    fn current_windows_win_without_confusing_model_or_surface_limits() {
        let data = json!({"five_hour":{"utilization":0}, "limits":[
            {"kind":"session","percent":7,"is_active":false},
            {"kind":"session","percent":19,"is_active":true},
            {"kind":"weekly_all","percent":99,"scope":{"model":{"display_name":"Model"}}},
            {"kind":"weekly_all","percent":88,"scope":{"surface":{"display_name":"Other"}}},
            {"kind":"weekly_all","percent":2,"is_active":true}
        ]});
        let windows = usage_observations(&data, NOW, "cli", NOW);
        assert_eq!(windows["five_hour"].window.utilization, 19.0);
        assert_eq!(windows["seven_day"].window.utilization, 2.0);
    }

    #[test]
    fn profile_and_credential_switches_invalidate_observations() {
        let (_dir, profile) = fixture();
        assert!(profile.unchanged());
        std::fs::write(
            &profile.credentials_path,
            r#"{"claudeAiOauth":{"accessToken":"different-test-token"}}"#,
        )
        .unwrap();
        assert!(!profile.unchanged());
        let new = Profile::at(
            profile.config_path.clone(),
            profile.credentials_path.clone(),
        )
        .unwrap();
        assert_ne!(profile.identity, new.identity);
        let other = tempfile::tempdir().unwrap();
        std::fs::copy(&profile.config_path, other.path().join("config")).unwrap();
        let moved = Profile::at(
            other.path().join("config"),
            profile.credentials_path.clone(),
        )
        .unwrap();
        assert_ne!(new.identity, moved.identity);
    }

    #[test]
    fn sdk_account_must_match_email_organization_and_provider() {
        let (_dir, profile) = fixture();
        let account = json!({"email":"a@example.test", "organization":"Test organization", "apiProvider":"firstParty"});
        assert!(profile.matches_account(&account));
        for (key, value) in [
            ("email", "b@example.test"),
            ("organization", "Other"),
            ("apiProvider", "bedrock"),
        ] {
            let mut wrong = account.clone();
            wrong[key] = json!(value);
            assert!(!profile.matches_account(&wrong));
        }
    }

    #[test]
    fn seeded_cli_responses_cannot_freshen_cached_quota() {
        let (_, profile) = fixture();
        assert_eq!(
            observation_time(&profile.config, &profile.account_uuid, NOW),
            Some(NOW - 1000)
        );
        assert_eq!(observation_time(&profile.config, "other", NOW), None);
        let mut stale = profile.config.clone();
        stale["cachedUsageUtilization"]["fetchedAtMs"] = json!(NOW - 3_600_000);
        let at = observation_time(&stale, &profile.account_uuid, NOW).unwrap();
        assert!(
            usage_observations(&json!({"five_hour":{"utilization":19}}), at, "cli", NOW).is_empty()
        );
    }

    #[test]
    fn sparse_updates_preserve_each_windows_age_and_never_roll_back() {
        let mut old = usage_observations(
            &json!({"five_hour":{"utilization":18},"seven_day":{"utilization":1}}),
            NOW - 120_000,
            "cli",
            NOW,
        );
        merge(
            &mut old,
            usage_observations(&json!({"five_hour":{"utilization":19}}), NOW, "cli", NOW),
        );
        merge(
            &mut old,
            usage_observations(
                &json!({"five_hour":{"utilization":17}}),
                NOW - 1000,
                "cli",
                NOW,
            ),
        );
        let result = reading(old).unwrap();
        assert_eq!(result.fetched_at_ms, NOW - 120_000);
        assert_eq!(result.data["five_hour"].utilization, 19.0);
        assert_eq!(result.data["seven_day"].utilization, 1.0);
    }

    #[test]
    fn private_cache_rejects_wrong_identity_and_expires_windows_independently() {
        let (dir, profile) = fixture();
        let path = dir.path().join("observations.json");
        let mut windows = cached_observations(&profile, NOW);
        windows.get_mut("five_hour").unwrap().source = "cli".into();
        windows.get_mut("seven_day").unwrap().fetched_at_ms = NOW - FRESH_MS - 1;
        storage::write_atomic(
            &path,
            &serde_json::to_vec(
                &json!({"identity":profile.identity,"version":1,"windows":windows}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(load_observations(&path, "wrong", NOW).is_empty());
        let loaded = load_observations(&path, &profile.identity, NOW);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key("five_hour"));
    }

    #[test]
    fn bounded_json_lines_recover_after_malformed_and_oversized_output() {
        let mut parser = JsonLines::default();
        let mut values = Vec::new();
        parser.push(&vec![b'x'; MAX_LINE + 1], |value| values.push(value));
        assert!(parser.line.is_empty());
        parser.push(b"\ninvalid\n{\"five_", |value| values.push(value));
        parser.push(b"hour\":19}\n", |value| values.push(value));
        assert_eq!(values, vec![json!({"five_hour":19})]);
    }

    #[test]
    fn control_requests_never_include_a_prompt_and_skip_transcript_scanning() {
        let init: Value = serde_json::from_slice(&control("init", "initialize")).unwrap();
        let usage: Value = serde_json::from_slice(&control("usage", "get_usage")).unwrap();
        assert_eq!(init["type"], "control_request");
        assert_eq!(init["request"], json!({"subtype":"initialize", "hooks":{}}));
        assert_eq!(
            usage["request"],
            json!({"subtype":"get_usage", "skip_behaviors":true})
        );
    }

    #[test]
    fn protocol_only_accepts_usage_after_verified_initialization() {
        let (_dir, profile) = fixture();
        let mut protocol = Protocol::default();
        let usage = json!({"type":"control_response","response":{"request_id":"usagestat-quota","subtype":"success","response":{"rate_limits_available":true,"rate_limits":{"five_hour":{"utilization":19}}}}});
        assert!(matches!(
            protocol.accept(&usage, &profile),
            process::StreamControl::Continue
        ));
        assert!(protocol.usage.is_none());
        let init = json!({"type":"control_response","response":{"request_id":"usagestat-init","subtype":"success","response":{"account":{"email":"a@example.test","organization":"Test organization","apiProvider":"firstParty"}}}});
        assert!(matches!(
            protocol.accept(&init, &profile),
            process::StreamControl::Reply(_)
        ));
        assert!(matches!(
            protocol.accept(&usage, &profile),
            process::StreamControl::Finish
        ));
        assert_eq!(protocol.usage.unwrap()["five_hour"]["utilization"], 19);
        let mut wrong = init.clone();
        wrong["response"]["response"]["account"]["email"] = json!("someone-else@example.test");
        let mut protocol = Protocol::default();
        assert!(matches!(
            protocol.accept(&wrong, &profile),
            process::StreamControl::Finish
        ));
        protocol.accept(&usage, &profile);
        assert!(protocol.usage.is_none());
    }
}
