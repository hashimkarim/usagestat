//! Persisted daemon intent, independent of a platform's login service manager.
use crate::storage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum T3Mode {
    Auto,
    #[default]
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    /// Stable installation directory, not a PID or a temporary npm cache.
    pub owner: PathBuf,
    pub binary: PathBuf,
    pub bind: SocketAddr,
    pub config: PathBuf,
    pub plugin_dirs: Vec<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub management_key_file: PathBuf,
    pub control_key_file: PathBuf,
}

impl Installation {
    pub fn base_url(&self) -> String {
        local_url(self.bind)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonSettings {
    pub t3_mode: T3Mode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installation: Option<Installation>,
}

impl DaemonSettings {
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stored {
            t3_mode: Option<T3Mode>,
            t3_enabled: Option<bool>,
            installation: Option<Installation>,
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let stored: Stored = serde_json::from_str(&text).map_err(invalid_data)?;
        let t3_mode = stored
            .t3_mode
            .or_else(|| {
                stored
                    .t3_enabled
                    .map(|enabled| if enabled { T3Mode::Auto } else { T3Mode::Off })
            })
            .ok_or_else(|| invalid_data("daemon settings must contain t3Mode (auto or off)"))?;
        Ok(Some(Self {
            t3_mode,
            installation: stored.installation,
        }))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(invalid_data)?;
        bytes.push(b'\n');
        storage::write_atomic(path, &bytes)
    }
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

pub fn local_url(bind: SocketAddr) -> String {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}", SocketAddr::new(ip, bind.port()))
}
