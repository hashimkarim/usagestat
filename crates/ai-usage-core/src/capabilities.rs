//! Additive client contract; implementation, runtime discovery and qualification
//! are separate. Constructing this report never probes a provider or OS store.
use crate::{ProviderSummary, paths, process};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Feature {
    pub implemented: bool,
    pub runtime: &'static str,
    pub qualification: &'static str,
    pub reason_code: Option<&'static str>,
}
impl Feature {
    fn new(implemented: bool, qualification: &'static str) -> Self {
        Self {
            implemented,
            runtime: if implemented {
                "not-checked"
            } else {
                "unsupported"
            },
            qualification,
            reason_code: (!implemented).then_some("platform-unsupported"),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSupport {
    pub id: String,
    pub declared_sources: Vec<String>,
    pub auto_source: String,
    pub qualification: &'static str,
    pub authentication: &'static str,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub schema_version: u32,
    pub backend_version: &'static str,
    pub api_version: u32,
    pub os: String,
    pub architecture: String,
    pub profile: String,
    pub service_manager: &'static str,
    pub features: BTreeMap<&'static str, Feature>,
    pub helpers: BTreeMap<&'static str, &'static str>,
    pub providers: Vec<ProviderSupport>,
    pub diagnostic_states: Vec<&'static str>,
}
pub fn current(providers: &[ProviderSummary]) -> Capabilities {
    let mut result = for_platform(
        std::env::consts::OS,
        std::env::consts::ARCH,
        paths::app_dir_name(),
        providers,
    );
    for helper in ["gh", "firectl", "node", "npx"] {
        result.helpers.insert(
            helper,
            if process::command(helper).is_ok() {
                "found"
            } else {
                "not-found"
            },
        );
    }
    if cfg!(target_os = "linux") {
        result.helpers.insert(
            "secret-tool",
            if process::command("secret-tool").is_ok() {
                "found"
            } else {
                "not-found"
            },
        );
    }
    result
}
fn for_platform(
    os: &str,
    architecture: &str,
    profile: &str,
    providers: &[ProviderSummary],
) -> Capabilities {
    let native = matches!(os, "linux" | "macos" | "windows");
    let mut features = BTreeMap::from([
        ("daemon.foreground", Feature::new(native, "native-fixtures")),
        (
            "daemon.autostart",
            Feature::new(native, "native-fixtures-session-pending"),
        ),
        (
            "daemon.authenticatedShutdown",
            Feature::new(native, "native-fixtures"),
        ),
        (
            "daemon.unregister",
            Feature::new(native, "native-fixtures"),
        ),
        (
            "daemon.independentControls",
            Feature::new(native, "native-fixtures"),
        ),
        (
            "credentials.genericPassword",
            Feature::new(
                native,
                if os == "windows" {
                    "native-fixtures"
                } else {
                    "unverified"
                },
            ),
        ),
        (
            "credentials.genericItemAccount",
            Feature::new(matches!(os, "macos" | "windows"), "unverified"),
        ),
        (
            "credentials.internetPassword",
            Feature::new(os == "macos", "unverified"),
        ),
        (
            "credentials.windowsExactTarget",
            Feature::new(os == "windows", "native-fixtures"),
        ),
        (
            "browser.automaticImport",
            Feature::new(native, "native-fixtures"),
        ),
        (
            "browser.chromiumCbcImport",
            Feature::new(matches!(os, "linux" | "macos"), "native-fixtures"),
        ),
        (
            "browser.windowsDpapiImport",
            Feature::new(os == "windows", "native-fixtures"),
        ),
        (
            "browser.appBoundImport",
            Feature { reason_code: Some("browser-app-bound-unsupported"), ..Feature::new(false, "unsupported-format") },
        ),
        (
            "browser.manualCredentials",
            Feature::new(native, "provider-qualification-pending"),
        ),
        (
            "ide.languageServerDiscovery",
            Feature::new(native, "native-fixtures"),
        ),
        (
            "helpers.processTreeCleanup",
            Feature::new(native, "native-fixtures"),
        ),
        (
            "providers.structuredStates",
            Feature::new(true, "contract-fixtures"),
        ),
    ]);
    for name in [
        "daemon.foreground",
        "daemon.authenticatedShutdown",
        "helpers.processTreeCleanup",
        "providers.structuredStates",
    ] {
        if let Some(feature) = features.get_mut(name).filter(|feature| feature.implemented) {
            feature.runtime = "available";
        }
    }
    Capabilities {
        schema_version: 1,
        backend_version: env!("CARGO_PKG_VERSION"),
        api_version: 1,
        os: os.into(),
        architecture: architecture.into(),
        profile: profile.into(),
        service_manager: match os {
            "linux" => "systemd",
            "macos" => "launchd",
            "windows" => "task-scheduler",
            _ => "none",
        },
        features,
        helpers: BTreeMap::new(),
        providers: providers
            .iter()
            .map(|provider| ProviderSupport {
                id: provider.id.clone(),
                declared_sources: provider.supported_modes.clone(),
                auto_source: provider.auto_mode.clone(),
                qualification: "unverified",
                authentication: "not-checked",
            })
            .collect(),
        diagnostic_states: vec![
            "ready",
            "unsupported",
            "missing-auth",
            "no-data",
            "credential-denied",
            "credential-unavailable",
            "credential-account-mismatch",
            "credential-malformed",
            "timed-out",
            "failed",
            "service-stopped",
            "wrong-version",
            "unhealthy",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn platform_contract_keeps_unsupported_methods_distinct_from_unchecked_credentials() {
        for (os, manager) in [
            ("linux", "systemd"),
            ("macos", "launchd"),
            ("windows", "task-scheduler"),
        ] {
            let value = for_platform(os, "x86_64", "usagestat-dev", &[]);
            assert_eq!(value.schema_version, 1);
            assert_eq!(value.service_manager, manager);
            assert_eq!(
                value.features["credentials.genericPassword"].runtime,
                "not-checked"
            );
            assert_eq!(
                value.features["credentials.internetPassword"].implemented,
                os == "macos"
            );
            assert!(value.features["browser.automaticImport"].implemented);
            assert_eq!(value.features["browser.automaticImport"].runtime, "not-checked");
            assert_eq!(value.features["browser.windowsDpapiImport"].implemented, os == "windows");
            assert_eq!(value.features["browser.chromiumCbcImport"].implemented, os != "windows");
            assert!(!value.features["browser.appBoundImport"].implemented);
            assert_eq!(value.features["browser.appBoundImport"].reason_code, Some("browser-app-bound-unsupported"));
            assert_eq!(value.profile, "usagestat-dev");
        }
        assert!(
            !for_platform("other", "unknown", "usagestat", &[]).features["daemon.autostart"]
                .implemented
        );
    }
}
