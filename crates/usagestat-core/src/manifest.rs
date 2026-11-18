use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderManifest {
    pub id: String,
    pub name: String,
    pub entry: String,
    #[serde(default)]
    pub enabled_by_default: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub links: Vec<ProviderLink>,
    #[serde(default)]
    pub icon: Option<ProviderIconRef>,
    #[serde(default)]
    pub icon_monochrome: Option<bool>,
    #[serde(default)]
    pub icon_supports_current_color: Option<bool>,
    #[serde(default)]
    pub status_page_url: Option<String>,
    #[serde(default)]
    pub usage_dashboard_url: Option<String>,
    /// Explicit source modes this plugin supports (e.g. ["oauth", "web"]).
    /// Empty means the plugin hasn't declared modes — no validation is applied.
    #[serde(default)]
    pub supported_modes: Vec<String>,
    /// What "auto" resolves to for this plugin (e.g. "oauth").
    /// Empty string means unspecified / plugin decides internally.
    #[serde(default)]
    pub auto_mode: String,
    /// The primary web URL for this provider (e.g. "https://claude.ai").
    /// Used to locate browser cookies when running in web mode.
    #[serde(default)]
    pub web_url: Option<String>,
    #[serde(default)]
    pub setup_fields: Vec<ProviderSetting>,
    #[serde(default)]
    pub setup_help: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSetting {
    pub key: String,
    pub title: String,
    pub description: String,
    #[serde(rename = "type")]
    pub value_type: String,
}

impl ProviderManifest {
    pub fn resolved_icon(&self, plugin_dir: &Path) -> Option<ProviderIcon> {
        let icon = self
            .icon
            .clone()
            .unwrap_or_else(|| ProviderIconRef::Path("icon.svg".to_string()));

        match icon {
            ProviderIconRef::Path(path) => {
                let path = path.trim();
                if path.is_empty() {
                    return None;
                }
                let raw = PathBuf::from(path);
                let resolved = if raw.is_absolute() {
                    raw
                } else {
                    plugin_dir.join(raw)
                };
                if !resolved.is_file() {
                    return None;
                }
                let resolved = canonical_string(resolved);
                let color_path = sibling_icon_path(plugin_dir, "icon-color.svg");
                let kind = icon_kind_for_path(&PathBuf::from(&resolved));
                let raster = kind != "svg";
                let variants = icon_variants(resolved.clone(), kind.clone(), color_path.clone());
                Some(ProviderIcon {
                    kind,
                    path: Some(resolved.clone()),
                    url: None,
                    monochrome: self.icon_monochrome.unwrap_or(!raster),
                    supports_current_color: self.icon_supports_current_color.unwrap_or(!raster),
                    monochrome_path: Some(resolved),
                    color_path,
                    variants: Some(variants),
                })
            }
            ProviderIconRef::Object(icon) => {
                if icon.kind.as_deref() == Some("url") {
                    let url = icon.url.or(icon.path)?;
                    return Some(ProviderIcon {
                        kind: "url".to_string(),
                        path: None,
                        url: Some(url),
                        monochrome: icon.monochrome.unwrap_or(false),
                        supports_current_color: icon.supports_current_color.unwrap_or(false),
                        monochrome_path: None,
                        color_path: None,
                        variants: None,
                    });
                }

                let raw_path = icon.path?;
                let raw = PathBuf::from(raw_path.trim());
                let resolved = if raw.is_absolute() {
                    raw
                } else {
                    plugin_dir.join(raw)
                };
                if !resolved.is_file() {
                    return None;
                }
                let resolved = canonical_string(resolved);
                let color_path = sibling_icon_path(plugin_dir, "icon-color.svg");
                let kind = icon
                    .kind
                    .unwrap_or_else(|| icon_kind_for_path(&PathBuf::from(&resolved)));
                let raster = kind != "svg";
                let variants = icon_variants(resolved.clone(), kind.clone(), color_path.clone());
                Some(ProviderIcon {
                    kind,
                    path: Some(resolved.clone()),
                    url: None,
                    monochrome: icon.monochrome.or(self.icon_monochrome).unwrap_or(!raster),
                    supports_current_color: icon
                        .supports_current_color
                        .or(self.icon_supports_current_color)
                        .unwrap_or(!raster),
                    monochrome_path: Some(resolved),
                    color_path,
                    variants: Some(variants),
                })
            }
        }
    }

    pub fn resolved_status_page_url(&self) -> Option<String> {
        self.status_page_url.clone().or_else(|| {
            self.links
                .iter()
                .find(|link| link.label.to_ascii_lowercase().contains("status"))
                .map(|link| link.url.clone())
        })
    }

    pub fn resolved_usage_dashboard_url(&self) -> Option<String> {
        self.usage_dashboard_url.clone().or_else(|| {
            self.links
                .iter()
                .find(|link| {
                    let label = link.label.to_ascii_lowercase();
                    label.contains("usage") || label.contains("dashboard")
                })
                .map(|link| link.url.clone())
        })
    }

    /// Returns true if the requested mode is supported by this plugin.
    /// "auto" is always accepted. Plugins without declared modes accept everything.
    pub fn is_mode_supported(&self, mode: &str) -> bool {
        mode == "auto"
            || self.supported_modes.is_empty()
            || self.supported_modes.iter().any(|m| m == mode)
    }

    /// Returns a human-readable error if the mode is unsupported, or None if ok.
    pub fn check_mode(&self, mode: &str) -> Option<String> {
        if self.is_mode_supported(mode) {
            return None;
        }
        let available = self.supported_modes.join(", ");
        Some(format!(
            "Mode '{}' is not supported by the '{}' plugin. Supported modes: {}.",
            mode, self.id, available
        ))
    }
}

fn canonical_string(path: PathBuf) -> String {
    std::fs::canonicalize(&path)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn sibling_icon_path(plugin_dir: &Path, file_name: &str) -> Option<String> {
    let path = plugin_dir.join(file_name);
    path.is_file().then(|| canonical_string(path))
}

fn icon_kind_for_path(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "png".to_string(),
        "jpg" | "jpeg" => "jpeg".to_string(),
        "webp" => "webp".to_string(),
        _ => "svg".to_string(),
    }
}

fn icon_variants(
    monochrome_path: String,
    monochrome_kind: String,
    color_path: Option<String>,
) -> ProviderIconVariants {
    let monochrome_raster = monochrome_kind != "svg";
    ProviderIconVariants {
        monochrome: Some(ProviderIconVariant {
            kind: monochrome_kind,
            path: monochrome_path,
            monochrome: !monochrome_raster,
            supports_current_color: !monochrome_raster,
        }),
        color: color_path.map(|path| ProviderIconVariant {
            kind: "svg".to_string(),
            path,
            monochrome: false,
            supports_current_color: false,
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ProviderIconRef {
    Path(String),
    Object(ProviderIconRefObject),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderIconRefObject {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub monochrome: Option<bool>,
    #[serde(default)]
    pub supports_current_color: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderIcon {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub monochrome: bool,
    pub supports_current_color: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monochrome_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variants: Option<ProviderIconVariants>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderIconVariants {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monochrome: Option<ProviderIconVariant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<ProviderIconVariant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderIconVariant {
    pub kind: String,
    pub path: String,
    pub monochrome: bool,
    pub supports_current_color: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct LoadedProvider {
    pub manifest: ProviderManifest,
    pub dir: PathBuf,
    pub entry_script: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub id: String,
    /// Base plugin for an explicitly configured source instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_parent: Option<String>,
    pub name: String,
    pub enabled: bool,
    /// Supported source modes declared by the plugin manifest.
    pub supported_modes: Vec<String>,
    /// What "auto" resolves to for this plugin.
    pub auto_mode: String,
    /// The primary web URL for this provider, if web mode is supported.
    pub web_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_page_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_dashboard_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<ProviderIcon>,
}
