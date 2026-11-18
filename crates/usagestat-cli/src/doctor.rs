//! A read-only report. Do not probe providers, read keys, create directories or
//! offer implicit repairs here: collecting diagnostics must not prompt a store.
use anyhow::Result;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use usagestat_core::{AppConfig, UsageCache, capabilities, paths};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Check {
    id: String,
    code: String,
    action: &'static str,
}
fn add(
    checks: &mut Vec<Check>,
    id: impl Into<String>,
    code: impl Into<String>,
    action: &'static str,
) {
    checks.push(Check {
        id: id.into(),
        code: code.into(),
        action,
    });
}

pub(super) fn run(
    config_override: Option<&Path>,
    extra_dirs: &[PathBuf],
    json: bool,
) -> Result<()> {
    let mut checks = Vec::new();
    let config_path = config_override
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(paths::config_file);
    let mut resolved_paths = BTreeMap::new();
    for (name, path) in [("config", paths::config_dir()), ("data", paths::data_dir())] {
        match path {
            Ok(path) => {
                add(
                    &mut checks,
                    format!("paths.{name}"),
                    if path.is_dir() {
                        "ready"
                    } else {
                        "directory-missing"
                    },
                    "Directories are created when a command first needs to save data.",
                );
                resolved_paths.insert(name, path);
            }
            Err(_) => add(
                &mut checks,
                format!("paths.{name}"),
                "path-unavailable",
                "Set an absolute USAGESTAT_CONFIG_DIR and USAGESTAT_DATA_DIR, or restore this user's native profile directories.",
            ),
        }
    }
    let config = match config_path {
        Ok(path) => {
            let present = path.exists();
            match AppConfig::load_optional(&path) {
                Ok(config) => {
                    add(
                        &mut checks,
                        "config",
                        if present { "ready" } else { "config-missing" },
                        "An absent config uses defaults; create config.toml only when custom settings are needed.",
                    );
                    Some(config)
                }
                Err(_) => {
                    add(
                        &mut checks,
                        "config",
                        "config-invalid",
                        "Review the local config file. Diagnostic output omits its values.",
                    );
                    None
                }
            }
        }
        Err(_) => {
            add(
                &mut checks,
                "config",
                "path-unavailable",
                "Set an absolute USAGESTAT_CONFIG_DIR.",
            );
            None
        }
    };
    let mut providers = Vec::new();
    if let Some(config) = &config {
        match paths::plugin_dirs(config, extra_dirs) {
            Ok(dirs) => {
                providers = usagestat_plugins::discover_providers(&dirs);
                add(
                    &mut checks,
                    "resources",
                    if providers.is_empty() {
                        "resources-missing"
                    } else {
                        "ready"
                    },
                    "Install the complete package with bundled plugins, or pass --plugin-dir with the provider directory.",
                );
            }
            Err(_) => add(
                &mut checks,
                "resources",
                "path-unavailable",
                "Check the plugin directories and native profile paths.",
            ),
        }
    } else {
        add(
            &mut checks,
            "resources",
            "not-checked",
            "Fix config discovery before checking resources.",
        );
    }
    add(
        &mut checks,
        "backend",
        if super::daemon::find_binary(None).is_ok() {
            "ready"
        } else {
            "binary-missing"
        },
        "Install the matching usagestatd beside usagestat, including the .exe suffix on Windows.",
    );
    let service = match super::daemon::diagnostic_status() {
        Ok(service) => {
            let code = service["code"].as_str().unwrap_or("unhealthy");
            let action = match code {
                "ready" => "No service repair is needed.",
                "wrong-version" => {
                    "Update CLI and backend together, then run usagestat daemon enable to reload the owned installation."
                }
                "service-stopped" => {
                    "Run usagestat daemon enable if you want startup at login, or run usagestatd in the foreground."
                }
                "installation-owner-mismatch" => {
                    "Inspect usagestat daemon status and the existing installation before explicitly switching its owner."
                }
                "service-manager-unavailable" => {
                    "Use an interactive user login with the native service manager, or run usagestatd in the foreground."
                }
                _ => {
                    "Inspect usagestat daemon status and the native service logs; check whether another process occupies the saved port."
                }
            };
            add(&mut checks, "service", code, action);
            service
        }
        Err(_) => {
            add(
                &mut checks,
                "service",
                "service-settings-unavailable",
                "Check daemon.json and access to the current user's native service manager.",
            );
            serde_json::json!({"code": "service-settings-unavailable"})
        }
    };
    let mut cache_states: BTreeMap<String, usize> = BTreeMap::new();
    match paths::cache_file()
        .map_err(anyhow::Error::from)
        .and_then(|path| UsageCache::load_optional(&path).map_err(Into::into))
    {
        Ok(cache) => {
            for snapshot in cache.list() {
                let state = snapshot
                    .state
                    .and_then(|state| serde_json::to_value(state).ok())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".into());
                *cache_states.entry(state).or_default() += 1;
            }
            add(
                &mut checks,
                "cache",
                if cache_states.is_empty() {
                    "no-data"
                } else {
                    "ready"
                },
                "Cache state reflects earlier probes only. Doctor does not authenticate or refresh providers.",
            );
        }
        Err(_) => add(
            &mut checks,
            "cache",
            "cache-unavailable",
            "Check access and the local cache format; doctor does not change it.",
        ),
    }
    let summaries = super::provider_summaries(&providers, &config.unwrap_or_default());
    let mut capabilities = capabilities::current(&summaries);
    if let Some(feature) = capabilities.features.get_mut("daemon.autostart") {
        if feature.implemented {
            feature.runtime = match service["managerAvailable"].as_bool() {
                Some(true) => "available",
                Some(false) => "unavailable",
                None => "not-checked",
            };
            feature.reason_code =
                (feature.runtime == "unavailable").then_some("service-manager-unavailable");
        }
    }
    for (helper, availability) in &capabilities.helpers {
        add(
            &mut checks,
            format!("helper.{helper}"),
            *availability,
            "Optional: install this helper and expose it on PATH only if a selected provider needs it.",
        );
    }
    add(
        &mut checks,
        "credentials",
        "not-checked",
        "Credential stores are checked only during an explicit provider or authentication operation.",
    );
    add(
        &mut checks,
        "browser.automaticImport",
        capabilities.features["browser.automaticImport"].runtime,
        "Use provider-specific manual credentials when automatic browser import is unsupported.",
    );
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schemaVersion": 1, "readOnly": true, "paths": resolved_paths,
                "checks": checks, "service": service, "cachedProviderStates": cache_states,
                "capabilities": capabilities,
            }))?
        );
    } else {
        println!(
            "usagestat {} · {} {} · {}",
            capabilities.backend_version,
            capabilities.os,
            capabilities.architecture,
            capabilities.service_manager
        );
        for check in checks {
            println!("{}: {}", check.id, check.code);
            if check.code != "ready" && check.code != "found" {
                println!("  {}", check.action);
            }
        }
    }
    Ok(())
}
