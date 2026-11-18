use crate::UsageSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageCache {
    snapshots: BTreeMap<String, UsageSnapshot>,
}

impl UsageCache {
    pub fn load_optional(path: &Path) -> Result<Self, CacheError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let text = fs::read_to_string(path).map_err(|source| CacheError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| CacheError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), CacheError> {
        let text = serde_json::to_string_pretty(self).map_err(CacheError::Serialize)?;
        crate::storage::write_atomic(path, format!("{text}\n").as_bytes()).map_err(|source| {
            CacheError::Write {
                path: path.to_path_buf(),
                source,
            }
        })
    }

    pub fn upsert(&mut self, snapshot: UsageSnapshot) {
        self.snapshots
            .insert(snapshot.provider_id.clone(), snapshot);
    }

    pub fn get(&self, provider_id: &str) -> Option<&UsageSnapshot> {
        self.snapshots.get(provider_id)
    }

    pub fn list(&self) -> Vec<UsageSnapshot> {
        self.snapshots.values().cloned().collect()
    }
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("failed to read cache {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse cache {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to create cache directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to serialize cache: {0}")]
    Serialize(serde_json::Error),
    #[error("failed to write cache {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}
