//! Optional multi-machine totals through a shared folder (Syncthing, Dropbox,
//! a network drive). Off unless `[sync]` names a directory.

use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

/// File schema written to `<dir>/<machine>.json`.
pub const SCHEMA: &str = "usagestat.sync.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    /// Shared folder. `~` expands to the home directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// This machine's name in the folder. Defaults to the host name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

impl SyncConfig {
    pub fn directory(&self) -> Option<PathBuf> {
        self.dir
            .as_deref()
            .filter(|d| !d.trim().is_empty())
            .map(crate::paths::expand_home)
    }

    /// A file-name-safe machine name: the configured one, else the host name.
    pub fn machine_name(&self) -> String {
        let raw = self.machine.clone().unwrap_or_else(host_name);
        let clean: String = raw
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '-'
                }
            })
            .take(64)
            .collect();
        let clean = clean.trim_matches('.').to_string();
        if clean.is_empty() {
            "this-machine".into()
        } else {
            clean
        }
    }
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this-machine".into())
}

/// A malformed `[sync]` table turns sync off instead of failing the config.
pub(crate) fn deserialize_sync<'de, D>(deserializer: D) -> Result<Option<SyncConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| v.try_into::<SyncConfig>().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_names_are_safe_file_names_and_bad_tables_turn_sync_off() {
        let named = SyncConfig {
            dir: Some("~/Sync".into()),
            machine: Some("../My Laptop/".into()),
        };
        assert_eq!(named.machine_name(), "-My-Laptop-");
        assert!(named.directory().unwrap().ends_with("Sync"));
        assert_eq!(SyncConfig::default().directory(), None);
        let config: crate::AppConfig = toml::from_str("[sync]\ndir = 5\n").unwrap();
        assert_eq!(config.sync, None);
        let config: crate::AppConfig =
            toml::from_str("[sync]\ndir = \"/tmp/x\"\nmachine = \"desk\"\n").unwrap();
        assert_eq!(config.sync.unwrap().machine_name(), "desk");
    }
}
