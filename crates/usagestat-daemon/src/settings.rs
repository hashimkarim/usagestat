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
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike};
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
        "setupFields":p.manifest.setup_fields,
        "setupHelp":p.manifest.setup_help,
    })).collect();
    json!({"schemaVersion":1,"revision":revision(raw.as_bytes()),"refreshSec":config.refresh_sec,
        "effectiveRefreshSec":refresh_override.unwrap_or(config.refresh_sec),"refreshOverride":refresh_override,
        "pluginDirs":config.plugin_dirs,"providers":entries,"catalog":catalog})
}

fn json_to_toml(value: &Value) -> Result<toml_edit::Value, ()> {
    match value {
        Value::String(s) if s.len() <= 16384 && !s.contains('\0') => Ok(s.as_str().into()),
        Value::Bool(v) => Ok((*v).into()),
        Value::Number(v) => v
            .as_i64()
            .map(toml_edit::Value::from)
            .or_else(|| {
                v.as_f64()
                    .filter(|v| v.is_finite())
                    .map(toml_edit::Value::from)
            })
            .ok_or(()),
        _ => Err(()),
    }
}

/// Replace a value in place, keeping its position and surrounding comments.
fn set_value(table: &mut dyn TableLike, key: &str, mut value: toml_edit::Value) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) => {
            *value.decor_mut() = existing.decor().clone();
            *existing = value;
        }
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

fn text_of(table: &dyn TableLike, key: &str) -> Option<String> {
    table.get(key).and_then(Item::as_str).map(str::to_owned)
}

/// Provider entries may be written as `[[providers]]` tables or as an inline array.
fn provider_entries(doc: &mut DocumentMut) -> Result<Vec<&mut dyn TableLike>, ()> {
    match doc.get_mut("providers") {
        None => Ok(Vec::new()),
        Some(Item::ArrayOfTables(tables)) => {
            Ok(tables.iter_mut().map(|t| t as &mut dyn TableLike).collect())
        }
        Some(Item::Value(toml_edit::Value::Array(array))) => array
            .iter_mut()
            .map(|v| {
                v.as_inline_table_mut()
                    .map(|t| t as &mut dyn TableLike)
                    .ok_or(())
            })
            .collect(),
        Some(_) => Err(()),
    }
}

fn push_provider(doc: &mut DocumentMut, id: &str) -> Result<(), ()> {
    match doc
        .entry("providers")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
    {
        Item::ArrayOfTables(tables) => {
            let mut table = Table::new();
            table.insert("id", toml_edit::value(id));
            table.insert("enabled", toml_edit::value(false));
            tables.push(table);
        }
        Item::Value(toml_edit::Value::Array(array)) => {
            let mut table = InlineTable::new();
            table.insert("id", id.into());
            table.insert("enabled", false.into());
            array.push(table);
        }
        _ => return Err(()),
    }
    Ok(())
}

fn remove_provider(doc: &mut DocumentMut, index: usize) {
    match doc.get_mut("providers") {
        Some(Item::ArrayOfTables(tables)) => tables.remove(index),
        Some(Item::Value(toml_edit::Value::Array(array))) => {
            array.remove(index);
        }
        _ => {}
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
    // Edit the document in place so user ordering and comments survive a save.
    let mut doc: DocumentMut = raw.parse().map_err(|_| ())?;
    if let Some(value) = patch.get("refreshSec") {
        let seconds = value
            .as_u64()
            .filter(|v| (5..=86400).contains(v))
            .ok_or(())?;
        set_value(doc.as_table_mut(), "refreshSec", (seconds as i64).into());
    }
    if let Some(value) = patch.get("pluginDirs") {
        let paths = value.as_array().filter(|v| v.len() <= 32).ok_or(())?;
        let paths: toml_edit::Array = paths
            .iter()
            .map(|p| {
                p.as_str()
                    .filter(|p| !p.is_empty() && p.len() <= 4096 && !p.contains('\0'))
                    .ok_or(())
            })
            .collect::<Result<_, _>>()?;
        set_value(doc.as_table_mut(), "pluginDirs", paths.into());
    }
    if let Some(id) = patch.get("removeProvider") {
        let id = id.as_str().ok_or(())?;
        let index = provider_entries(&mut doc)?
            .iter()
            .position(|p| text_of(&**p, "instanceId").as_deref() == Some(id))
            .ok_or(())?;
        remove_provider(&mut doc, index);
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
        let find = |doc: &mut DocumentMut| -> Result<Option<usize>, ()> {
            Ok(provider_entries(doc)?.iter().position(|p| {
                text_of(&**p, "id").as_deref() == Some(id)
                    && text_of(&**p, "instanceId").as_deref() == instance
            }))
        };
        let index = match find(&mut doc)? {
            Some(index) => index,
            None => {
                push_provider(&mut doc, id)?;
                provider_entries(&mut doc)?.len() - 1
            }
        };
        let mut entries = provider_entries(&mut doc)?;
        let entry = &mut *entries[index];
        for (key, value) in fields {
            if key == "settings" {
                let settings = value.as_object().filter(|s| s.len() <= 128).ok_or(())?;
                if entry.get(key).is_none() {
                    entry.insert(key, Item::Value(InlineTable::new().into()));
                }
                let target = entry
                    .get_mut(key)
                    .and_then(Item::as_table_like_mut)
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
                        set_value(target, name, json_to_toml(value)?);
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
            set_value(entry, key, json_to_toml(value)?);
        }
    }
    let text = doc.to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(id: &str) -> LoadedProvider {
        LoadedProvider {
            manifest: serde_json::from_value(json!({"id": id, "name": id, "entry": "plugin.js"}))
                .unwrap(),
            dir: PathBuf::from("."),
            entry_script: String::new(),
        }
    }

    const CONFIG: &str = r#"# Poll often while testing.
refreshSec = 60 # seconds

[[providers]]
enabled = true
id = "claude" # primary account

# Kept for later.
[[providers]]
id = "codex"
enabled = false
"#;

    #[test]
    fn saves_edit_values_in_place_and_keep_comments_and_order() {
        let providers = [loaded("claude"), loaded("codex"), loaded("zed")];
        let (text, config) = apply_patch(
            CONFIG,
            &json!({"refreshSec": 90, "provider": {"id": "codex", "enabled": true, "region": "eu"}}),
            &providers,
        )
        .unwrap();
        assert_eq!(config.refresh_sec, 90);
        assert_eq!(
            text,
            CONFIG
                .replace("refreshSec = 60 #", "refreshSec = 90 #")
                .replace(
                    "id = \"codex\"\nenabled = false\n",
                    "id = \"codex\"\nenabled = true\nregion = \"eu\"\n"
                )
        );

        let (text, _) = apply_patch(
            &text,
            &json!({"provider": {"id": "zed", "settings": {"quota": 27}}}),
            &providers,
        )
        .unwrap();
        assert!(text.starts_with("# Poll often while testing.\nrefreshSec = 90 # seconds\n"));
        assert!(text.contains("id = \"claude\" # primary account"));
        assert!(
            text.ends_with(
                "[[providers]]\nid = \"zed\"\nenabled = false\nsettings = { quota = 27 }\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn inline_provider_arrays_are_edited_without_rewriting_the_file() {
        let raw = "providers = [{ id = \"claude\", enabled = true }] # inline\n";
        let (text, config) = apply_patch(
            raw,
            &json!({"provider": {"id": "claude", "enabled": false}, "pluginDirs": ["/opt/plugins"]}),
            &[loaded("claude")],
        )
        .unwrap();
        assert!(!config.providers[0].enabled);
        assert_eq!(
            text,
            "providers = [{ id = \"claude\", enabled = false }] # inline\npluginDirs = [\"/opt/plugins\"]\n"
        );
    }
}
