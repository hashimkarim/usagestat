use crate::hooks::HookConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default = "default_refresh_sec")]
    pub refresh_sec: u64,
    #[serde(default)]
    pub plugin_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Event hooks run by the daemon. See `crate::hooks`.
    #[serde(
        default,
        deserialize_with = "crate::hooks::deserialize_hooks",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub hooks: Vec<HookConfig>,
    /// Optional monthly budgets for local agent usage. See `crate::budgets`.
    #[serde(
        default,
        deserialize_with = "crate::budgets::deserialize_budgets",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub budgets: Vec<crate::budgets::BudgetConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            refresh_sec: default_refresh_sec(),
            plugin_dirs: Vec::new(),
            providers: Vec::new(),
            hooks: Vec::new(),
            budgets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    /// Provider plugin id, such as `claude` or `codex`.
    pub id: String,
    /// Stable instance id for multiple configured instances of the same provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// Optional parent/group id for child sources shown under one provider tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_parent: Option<String>,
    /// User-facing label override for this provider instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Preferred source mode: auto, web, cli, oauth, api, local, custom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ProviderSource>,
    /// Command used by custom command-backed providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_command: Option<String>,
    /// Common credential/settings fields used by provider preference pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Provider-specific settings. Prefer this for new fields instead of widening
    /// the top-level schema for every provider-specific preference.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProviderSource {
    Auto,
    Web,
    Cli,
    Oauth,
    Api,
    Local,
    Custom,
}

impl ProviderSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderSource::Auto => "auto",
            ProviderSource::Web => "web",
            ProviderSource::Cli => "cli",
            ProviderSource::Oauth => "oauth",
            ProviderSource::Api => "api",
            ProviderSource::Local => "local",
            ProviderSource::Custom => "custom",
        }
    }
}

impl AppConfig {
    pub fn load_optional(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn is_enabled(&self, provider_id: &str, enabled_by_default: bool) -> bool {
        self.providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .map(|provider| provider.enabled)
            .unwrap_or(enabled_by_default)
    }

    pub fn source_mode(&self, provider_id: &str) -> &str {
        self.providers
            .iter()
            .find(|p| p.id == provider_id)
            .and_then(|p| p.source.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("auto")
    }

    pub fn provider_config(&self, provider_id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == provider_id)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

fn default_refresh_sec() -> u64 {
    60
}

fn default_true() -> bool {
    true
}
