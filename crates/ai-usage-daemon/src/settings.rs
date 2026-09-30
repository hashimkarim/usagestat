//! Local dashboard configuration. Secrets are write-only; display preferences
//! stay in the browser. This capability is separate from T3 and SDK credentials.
use crate::{AppState, cliproxy, http_request::Request, response_text};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use usagestat_core::{AppConfig, LoadedProvider, paths, storage};
use usagestat_plugins::discover_providers;

const PREFIX: &str = "/v1/settings";
const MAX_CONFIG: usize = 1024 * 1024;

pub struct SettingsApi {
    pub path: PathBuf,
    pub plugin_overrides: Vec<PathBuf>,
    pub refresh_override: Option<u64>,
    address: SocketAddr,
    token: String,
    setup_key: String,
    key_path: PathBuf,
    transaction: Mutex<()>,
}

impl std::fmt::Debug for SettingsApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsApi")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

fn response(status: u16, code: &str, body: Value) -> String {
    response_text(
        status,
        code,
        "application/json; charset=utf-8",
        &body.to_string(),
    )
}
fn failure(status: u16, reason: &str, code: &str) -> String {
    response(status, reason, json!({"error": code}))
}
fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn random_key() -> anyhow::Result<String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| anyhow::anyhow!("create dashboard capability"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "key",
        "token",
        "cookie",
        "secret",
        "password",
        "credential",
        "authorization",
        "curl",
    ]
    .iter()
    .any(|part| key.contains(part))
}

impl SettingsApi {
    pub fn new(
        path: PathBuf,
        plugin_overrides: Vec<PathBuf>,
        refresh_override: Option<u64>,
        address: SocketAddr,
    ) -> anyhow::Result<Self> {
        // Loopback is shared by every OS user. Only the private profile owner
        // may mint a configuration capability, especially for custom commands.
        let key_path = paths::data_dir()?.join("dashboard-setup.key");
        storage::create_once(&key_path, format!("{}\n", random_key()?).as_bytes())?;
        if std::fs::symlink_metadata(&key_path)?.len() > 512 {
            anyhow::bail!("invalid dashboard setup key file");
        }
        let setup_key = storage::read_private(&key_path)?.trim().to_owned();
        if setup_key.len() != 64 || !setup_key.bytes().all(|b| b.is_ascii_hexdigit()) {
            anyhow::bail!("invalid dashboard setup key file");
        }
        Ok(Self {
            path,
            plugin_overrides,
            refresh_override,
            address,
            token: random_key()?,
            setup_key,
            key_path,
            transaction: Mutex::new(()),
        })
    }

    fn same_origin(&self, request: &Request) -> bool {
        let Some(host) = request.header("host") else {
            return false;
        };
        let port = self.address.port();
        if ![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ]
        .contains(&host.to_ascii_lowercase())
        {
            return false;
        }
        if request.header("x-usagestat-dashboard") != Some("1") {
            return false;
        }
        if request.headers.iter().any(|(key, _)| key == "origin")
            && request.header("origin") != Some(format!("http://{host}").as_str())
        {
            return false;
        }
        if request
            .header("sec-fetch-site")
            .is_some_and(|site| site != "same-origin" && site != "none")
        {
            return false;
        }
        request.method == "GET"
            || request.header("origin") == Some(format!("http://{host}").as_str())
    }

    pub fn handles(path: &str) -> bool {
        path == PREFIX || path.starts_with("/v1/settings/")
    }

    pub fn route(
        &self,
        request: &Request,
        state: &Arc<Mutex<AppState>>,
        flag: &AtomicBool,
    ) -> String {
        if !self.same_origin(request) {
            return failure(403, "Forbidden", "dashboard_origin_required");
        }
        if request.path == "/v1/settings/session" && request.method == "GET" {
            let authorized = request
                .header("x-usagestat-session")
                .is_some_and(|key| cliproxy::keys_equal(&self.token, key))
                || request
                    .header("x-usagestat-setup-key")
                    .is_some_and(|key| cliproxy::keys_equal(&self.setup_key, key));
            if !authorized {
                return response(
                    401,
                    "Unauthorized",
                    json!({
                        "error":"dashboard_setup_key_required", "keyFile":self.key_path,
                    }),
                );
            }
            return response(200, "OK", json!({"token":self.token}));
        }
        if !request
            .header("x-usagestat-session")
            .is_some_and(|key| cliproxy::keys_equal(&self.token, key))
        {
            return failure(401, "Unauthorized", "dashboard_session_required");
        }
        let _transaction = self
            .transaction
            .lock()
            .expect("settings transaction poisoned");
        let Ok((raw, config)) = self.read() else {
            return failure(503, "Service Unavailable", "config_unavailable");
        };
        let Ok(dirs) = paths::plugin_dirs(&config, &self.plugin_overrides) else {
            return failure(503, "Service Unavailable", "plugins_unavailable");
        };
        let providers = discover_providers(&dirs);
        if request.path != PREFIX {
            return failure(404, "Not Found", "settings_route_not_found");
        }
        match request.method.as_str() {
            "GET" => response(200, "OK", self.view(&raw, &config, &providers)),
            "PATCH" => {
                if request
                    .header("content-type")
                    .map(|v| v.split(';').next().unwrap_or("").trim())
                    != Some("application/json")
                {
                    return failure(415, "Unsupported Media Type", "json_required");
                }
                let Ok(patch) = serde_json::from_slice::<Value>(&request.body) else {
                    return failure(400, "Bad Request", "invalid_settings");
                };
                if patch.get("revision").and_then(Value::as_str)
                    != Some(revision(raw.as_bytes()).as_str())
                {
                    return failure(409, "Conflict", "config_changed");
                }
                let Ok((text, updated)) = apply_patch(&raw, &patch, &providers) else {
                    return failure(400, "Bad Request", "invalid_settings");
                };
                // An external preferences writer may have changed the file since
                // this form was opened. Never publish its old credential values.
                if self.read().ok().is_none_or(|(current, _)| current != raw) {
                    return failure(409, "Conflict", "config_changed");
                }
                let Ok(dirs) = paths::plugin_dirs(&updated, &self.plugin_overrides) else {
                    return failure(503, "Service Unavailable", "plugins_unavailable");
                };
                let discovered = discover_providers(&dirs);
                if storage::write_atomic(&self.path, text.as_bytes()).is_err() {
                    return failure(503, "Service Unavailable", "config_write_failed");
                }
                state.lock().expect("app state poisoned").providers =
                    crate::provider_summaries(&discovered, &updated);
                flag.store(true, Ordering::Release);
                response(200, "OK", self.view(&text, &updated, &discovered))
            }
            _ => failure(405, "Method Not Allowed", "method_not_allowed"),
        }
    }

    fn read(&self) -> Result<(String, AppConfig), ()> {
        let raw = if self.path.exists() {
            if std::fs::symlink_metadata(&self.path).map_err(|_| ())?.len() > MAX_CONFIG as u64 {
                return Err(());
            }
            storage::read_private(&self.path).map_err(|_| ())?
        } else {
            String::new()
        };
        if raw.len() > MAX_CONFIG {
            return Err(());
        }
        let config = toml::from_str(&raw).map_err(|_| ())?;
        Ok((raw, config))
    }

    fn view(&self, raw: &str, config: &AppConfig, loaded: &[LoadedProvider]) -> Value {
        let document: toml::Value =
            toml::from_str(raw).unwrap_or(toml::Value::Table(Default::default()));
        let mut entries = document
            .get("providers")
            .and_then(toml::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for provider in loaded {
            if !entries.iter().any(|p| {
                p.get("id").and_then(toml::Value::as_str) == Some(provider.manifest.id.as_str())
            }) {
                entries.push(toml::Value::try_from(json!({"id":provider.manifest.id,"enabled":provider.manifest.enabled_by_default})).expect("provider defaults"));
            }
        }
        let entries: Vec<Value> = entries.iter().filter_map(|entry| {
            let table = entry.as_table()?;
            let mut value = json!({"enabled":true,"source":"auto","settings":{}});
            for key in ["id","instanceId","displayName","enabled","source","region","workspaceId","tabParent"] {
                if let Some(item) = table.get(key) { value[key] = serde_json::to_value(item).unwrap_or(Value::Null); }
            }
            for key in ["apiKey","cookieHeader","customCommand"] {
                value[format!("{key}Configured")] = json!(table.get(key).is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty())));
            }
            if let Some(settings) = table.get("settings").and_then(toml::Value::as_table) {
                for (key, item) in settings {
                    let hidden = secret(key) || item.is_array() || item.is_table();
                    value["settings"][key] = if hidden { json!({"secret":true,"configured":true,"type":"string"}) }
                        else { json!({"secret":false,"configured":true,"value":item,"type":if item.is_bool(){"boolean"}else if item.is_integer()||item.is_float(){"number"}else{"string"}}) };
                }
            }
            Some(value)
        }).collect();
        response_data(raw, config, self.refresh_override, entries, loaded)
    }
}

fn response_data(
    raw: &str,
    config: &AppConfig,
    refresh_override: Option<u64>,
    entries: Vec<Value>,
    loaded: &[LoadedProvider],
) -> Value {
    let catalog: Vec<_> = loaded.iter().map(|p| json!({
        "id":p.manifest.id,"name":p.manifest.name,"supportedModes":p.manifest.supported_modes,
        "autoMode":p.manifest.auto_mode,"webUrl":p.manifest.web_url,
    })).collect();
    json!({"schemaVersion":1,"revision":revision(raw.as_bytes()),"refreshSec":config.refresh_sec,
        "effectiveRefreshSec":refresh_override.unwrap_or(config.refresh_sec),"refreshOverride":refresh_override,
        "pluginDirs":config.plugin_dirs,"providers":entries,"catalog":catalog})
}

fn json_to_toml(value: &Value) -> Result<toml::Value, ()> {
    match value {
        Value::String(s) if s.len() <= 16384 && !s.contains('\0') => {
            Ok(toml::Value::String(s.clone()))
        }
        Value::Bool(v) => Ok(toml::Value::Boolean(*v)),
        Value::Number(v) => v
            .as_i64()
            .map(toml::Value::Integer)
            .or_else(|| v.as_f64().filter(|v| v.is_finite()).map(toml::Value::Float))
            .ok_or(()),
        _ => Err(()),
    }
}

fn apply_patch(
    raw: &str,
    patch: &Value,
    loaded: &[LoadedProvider],
) -> Result<(String, AppConfig), ()> {
    let patch = patch.as_object().ok_or(())?;
    if patch.keys().any(|k| {
        ![
            "revision",
            "refreshSec",
            "pluginDirs",
            "provider",
            "removeProvider",
        ]
        .contains(&k.as_str())
    }) {
        return Err(());
    }
    let mut doc: toml::Value = toml::from_str(raw).map_err(|_| ())?;
    let doc = doc.as_table_mut().ok_or(())?;
    if let Some(value) = patch.get("refreshSec") {
        let seconds = value
            .as_u64()
            .filter(|v| (5..=86400).contains(v))
            .ok_or(())?;
        doc.insert("refreshSec".into(), toml::Value::Integer(seconds as i64));
    }
    if let Some(value) = patch.get("pluginDirs") {
        let paths = value.as_array().filter(|v| v.len() <= 32).ok_or(())?;
        let paths: Vec<_> = paths
            .iter()
            .map(|p| {
                p.as_str()
                    .filter(|p| !p.is_empty() && p.len() <= 4096 && !p.contains('\0'))
                    .map(|p| toml::Value::String(p.into()))
                    .ok_or(())
            })
            .collect::<Result<_, _>>()?;
        doc.insert("pluginDirs".into(), toml::Value::Array(paths));
    }
    if let Some(id) = patch.get("removeProvider") {
        let id = id.as_str().ok_or(())?;
        let entries = doc
            .get_mut("providers")
            .and_then(toml::Value::as_array_mut)
            .ok_or(())?;
        let index = entries
            .iter()
            .position(|p| p.get("instanceId").and_then(toml::Value::as_str) == Some(id))
            .ok_or(())?;
        entries.remove(index);
    }
    if let Some(provider) = patch.get("provider") {
        let fields = provider.as_object().ok_or(())?;
        if fields.keys().any(|k| {
            ![
                "id",
                "instanceId",
                "displayName",
                "enabled",
                "source",
                "apiKey",
                "cookieHeader",
                "region",
                "workspaceId",
                "customCommand",
                "settings",
            ]
            .contains(&k.as_str())
        }) {
            return Err(());
        }
        let id = fields.get("id").and_then(Value::as_str).ok_or(())?;
        let manifest = loaded.iter().find(|p| p.manifest.id == id).ok_or(())?;
        let instance = match fields.get("instanceId") {
            Some(value) => Some(value.as_str().filter(|s| valid_id(s)).ok_or(())?),
            None => None,
        };
        let entries = doc
            .entry("providers")
            .or_insert(toml::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or(())?;
        let index = entries.iter().position(|p| {
            p.get("id").and_then(toml::Value::as_str) == Some(id)
                && p.get("instanceId").and_then(toml::Value::as_str) == instance
        });
        if index.is_none() {
            entries.push(toml::Value::try_from(json!({"id":id,"enabled":false})).map_err(|_| ())?);
        }
        let index = index.unwrap_or(entries.len() - 1);
        let entry = entries[index].as_table_mut().ok_or(())?;
        for (key, value) in fields {
            if key == "settings" {
                let settings = value.as_object().filter(|s| s.len() <= 128).ok_or(())?;
                let target = entry
                    .entry(key.clone())
                    .or_insert(toml::Value::Table(Default::default()))
                    .as_table_mut()
                    .ok_or(())?;
                for (name, value) in settings {
                    if !valid_id(name)
                        || ["__proto__", "prototype", "constructor"].contains(&name.as_str())
                    {
                        return Err(());
                    }
                    if value.is_null() {
                        target.remove(name);
                    } else {
                        target.insert(name.clone(), json_to_toml(value)?);
                    }
                }
                continue;
            }
            if value.is_null() && !["id", "instanceId", "enabled"].contains(&key.as_str()) {
                entry.remove(key);
                continue;
            }
            if key == "enabled" && !value.is_boolean() {
                return Err(());
            }
            if key != "enabled" && !value.is_string() {
                return Err(());
            }
            if key == "source" {
                let mode = value.as_str().ok_or(())?;
                if mode != "custom" && manifest.manifest.check_mode(mode).is_some() {
                    return Err(());
                }
            }
            entry.insert(key.clone(), json_to_toml(value)?);
        }
    }
    let text = toml::to_string_pretty(doc).map_err(|_| ())?;
    if text.len() > MAX_CONFIG {
        return Err(());
    }
    let config: AppConfig = toml::from_str(&text).map_err(|_| ())?;
    let mut keys = std::collections::HashSet::new();
    for provider in &config.providers {
        if provider
            .instance_id
            .as_ref()
            .is_some_and(|id| loaded.iter().any(|p| &p.manifest.id == id))
        {
            return Err(());
        }
        if !valid_id(&provider.id)
            || provider
                .instance_id
                .as_ref()
                .is_some_and(|id| !valid_id(id))
            || !keys.insert(provider.instance_id.as_deref().unwrap_or(&provider.id))
        {
            return Err(());
        }
        if provider
            .source
            .as_ref()
            .is_some_and(|v| v.as_str() == "custom")
            && provider.enabled
            && provider
                .custom_command
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err(());
        }
    }
    Ok((text, config))
}
