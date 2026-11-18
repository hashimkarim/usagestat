use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use usagestat_core::{LoadedProvider, ProviderManifest};

pub fn discover_providers(plugin_dirs: &[PathBuf]) -> Vec<LoadedProvider> {
    let mut providers = Vec::new();
    let mut seen_ids = HashSet::new();

    for plugin_dir in plugin_dirs {
        let Ok(entries) = fs::read_dir(plugin_dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            match load_provider(&path) {
                Ok(provider) => {
                    if seen_ids.insert(provider.manifest.id.clone()) {
                        providers.push(provider);
                    }
                }
                Err(error) => {
                    log::warn!("failed to load provider plugin {}: {error}", path.display())
                }
            }
        }
    }

    providers.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    providers
}

pub fn load_provider(dir: &Path) -> Result<LoadedProvider> {
    let manifest_path = dir.join("plugin.json");
    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: ProviderManifest = serde_json::from_str(&manifest_text)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let entry_path = dir.join(&manifest.entry);
    let entry_script = fs::read_to_string(&entry_path)
        .with_context(|| format!("read {}", entry_path.display()))?;

    Ok(LoadedProvider {
        manifest,
        dir: dir.to_path_buf(),
        entry_script,
    })
}
