use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::AppConfig;

#[derive(Debug, thiserror::Error)]
#[error(
    "native {kind} directory is unavailable; set {override_name} to an explicit application directory"
)]
pub struct PathError {
    kind: &'static str,
    override_name: &'static str,
}

pub fn app_dir_name() -> &'static str {
    profile_name(std::env::current_exe().ok().as_deref())
}

fn profile_name(exe: Option<&Path>) -> &'static str {
    if exe
        .and_then(Path::file_stem)
        .is_some_and(|name| name == "usagestat-dev" || name == "usagestatd-dev")
    {
        "usagestat-dev"
    } else {
        "usagestat"
    }
}

/// Profile identity belongs to the executable, independently of path overrides.
pub fn is_dev_profile() -> bool {
    app_dir_name() == "usagestat-dev"
}

/// Use the native profile directory, including Windows Known Folder redirection.
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

pub fn expand_home(path: &str) -> PathBuf {
    expand_home_with(path, home_dir().as_deref(), cfg!(windows))
}

fn expand_home_with(path: &str, home: Option<&Path>, windows: bool) -> PathBuf {
    if let Some(home) = home {
        if path == "~" {
            return home.to_owned();
        }
        if let Some(rest) = path
            .strip_prefix("~/")
            .or_else(|| windows.then(|| path.strip_prefix("~\\")).flatten())
        {
            return home.join(rest);
        }
    }
    // Leave unavailable homes literal for the caller to diagnose; never turn
    // a missing home into the working directory.
    PathBuf::from(path)
}

fn app_path(
    override_dir: Option<OsString>,
    native_dir: Option<PathBuf>,
    profile: &str,
    kind: &'static str,
    override_name: &'static str,
) -> Result<PathBuf, PathError> {
    if let Some(value) = override_dir.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    native_dir.map(|dir| dir.join(profile)).ok_or(PathError {
        kind,
        override_name,
    })
}

/// USAGESTAT_CONFIG_DIR names the complete application directory, not its parent.
pub fn config_dir() -> Result<PathBuf, PathError> {
    app_path(
        std::env::var_os("USAGESTAT_CONFIG_DIR"),
        dirs::config_dir(),
        app_dir_name(),
        "configuration",
        "USAGESTAT_CONFIG_DIR",
    )
}

/// Machine-local state must not roam between Windows machines.
pub fn data_dir() -> Result<PathBuf, PathError> {
    app_path(
        std::env::var_os("USAGESTAT_DATA_DIR"),
        dirs::data_local_dir(),
        app_dir_name(),
        "data",
        "USAGESTAT_DATA_DIR",
    )
}

pub fn config_file() -> Result<PathBuf, PathError> {
    Ok(config_dir()?.join("config.toml"))
}

/// Secrets remain at the established config location on Unix. Windows keeps
/// them in local AppData rather than roaming application settings.
pub fn private_state_dir() -> Result<PathBuf, PathError> {
    if cfg!(windows) {
        data_dir()
    } else {
        config_dir()
    }
}

pub fn management_key_file() -> Result<PathBuf, PathError> {
    Ok(private_state_dir()?.join("t3-management-key"))
}

pub fn control_key_file() -> Result<PathBuf, PathError> {
    Ok(private_state_dir()?.join("daemon-control-key"))
}

pub fn cache_file() -> Result<PathBuf, PathError> {
    Ok(data_dir()?.join("snapshots.json"))
}

pub fn usage_daily_file() -> Result<PathBuf, PathError> {
    Ok(data_dir()?.join("usage_daily.json"))
}

pub fn default_plugin_dirs() -> Result<Vec<PathBuf>, PathError> {
    let mut dirs = Vec::new();

    if let Some(value) = std::env::var_os("USAGESTAT_PLUGIN_DIR")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("AI_USAGE_PLUGIN_DIR").filter(|value| !value.is_empty()))
    {
        dirs.push(PathBuf::from(value));
    }

    dirs.push(config_dir()?.join("plugins"));
    if is_dev_profile() {
        dirs.push(data_dir()?.join("plugins"));
    }
    if let Ok(exe) = std::env::current_exe() {
        dirs.extend(installed_plugin_dirs(&exe, app_dir_name()));
    }
    dirs.push(PathBuf::from("plugins"));
    Ok(dirs)
}

fn installed_plugin_dirs(exe: &Path, profile: &str) -> Vec<PathBuf> {
    let Some(bin_dir) = exe.parent() else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    if bin_dir.file_name().is_some_and(|name| name == "bin") {
        if let Some(prefix) = bin_dir.parent() {
            dirs.push(prefix.join("share").join(profile).join("plugins"));
            dirs.push(prefix.join("lib").join(profile).join("plugins"));
            // Native npm packages may separate bin/ and plugins/.
            dirs.push(prefix.join("plugins"));
        }
    }
    if bin_dir.file_name().is_some_and(|name| name == "MacOS") {
        if let Some(contents) = bin_dir
            .parent()
            .filter(|dir| dir.file_name().is_some_and(|name| name == "Contents"))
        {
            dirs.push(contents.join("Resources").join("plugins"));
        }
    }
    // Flat archives and installer layouts work from any launch directory.
    dirs.push(bin_dir.join("plugins"));
    dirs
}

/// Replace only known bundled-resource locations when an installation moves.
/// External/custom plugin roots retain their order. This works after an old keg
/// was removed, so matching must not depend on the old paths still existing.
pub fn relocate_installed_plugin_dirs(
    saved: &[PathBuf],
    old_executable: &Path,
    new_executable: &Path,
    profile: &str,
) -> Vec<PathBuf> {
    if old_executable == new_executable {
        return saved.to_vec();
    }
    let previous = installed_plugin_dirs(old_executable, profile);
    let replacement = installed_plugin_dirs(new_executable, profile);
    let mut result = Vec::new();
    let mut replaced = false;
    for path in saved {
        if previous.contains(path) {
            if !replaced {
                result.extend(replacement.iter().cloned());
                replaced = true;
            }
        } else {
            result.push(path.clone());
        }
    }
    dedupe_dirs(result)
}

pub fn plugin_dirs(config: &AppConfig, extra_dirs: &[PathBuf]) -> Result<Vec<PathBuf>, PathError> {
    let mut dirs = Vec::new();
    dirs.extend(extra_dirs.iter().cloned());
    dirs.extend(config.plugin_dirs.iter().cloned());
    dirs.extend(default_plugin_dirs()?);
    Ok(dedupe_dirs(dirs))
}

fn dedupe_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        let already_seen = out.iter().any(|existing| {
            let existing_canonical =
                std::fs::canonicalize(existing).unwrap_or_else(|_| existing.clone());
            existing_canonical == canonical
        });
        if !already_seen {
            out.push(dir);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_relocate_bundled_paths_after_old_keg_removal_preserving_custom_order() {
        let old = Path::new("Cellar/usagestat/1.0.3/bin/usagestatd");
        let new = Path::new("Cellar/usagestat/1.0.4/bin/usagestatd");
        let custom = PathBuf::from("explicit 使用 plugins");
        let mut saved = vec![custom.clone()];
        saved.extend(installed_plugin_dirs(old, "usagestat"));
        saved.push(PathBuf::from("outside/other-package/plugins"));
        let result = relocate_installed_plugin_dirs(&saved, old, new, "usagestat");
        let mut expected = vec![custom.clone()];
        expected.extend(installed_plugin_dirs(new, "usagestat"));
        expected.push(PathBuf::from("outside/other-package/plugins"));
        assert_eq!(result, expected);
        assert_eq!(
            relocate_installed_plugin_dirs(&saved, old, old, "usagestat"),
            saved
        );
        assert_eq!(
            relocate_installed_plugin_dirs(&[custom.clone()], old, new, "usagestat"),
            vec![custom]
        );
        // Explicit owner transfer can change the resource layout too.
        let flat = Path::new("new archive 使用/usagestatd.exe");
        let result = relocate_installed_plugin_dirs(&saved, old, flat, "usagestat");
        assert_eq!(result[1], PathBuf::from("new archive 使用/plugins"));
        assert_eq!(result.len(), 3);
        let app = Path::new("Usage.app/Contents/MacOS/usagestatd");
        let result = relocate_installed_plugin_dirs(&saved, old, app, "usagestat");
        assert_eq!(
            result[1],
            PathBuf::from("Usage.app/Contents/Resources/plugins")
        );
    }

    #[test]
    fn home_expansion_preserves_native_paths_and_missing_homes() {
        let home = Path::new("redirected 使用/home");
        assert_eq!(
            expand_home_with("~/logs with spaces", Some(home), false),
            home.join("logs with spaces")
        );
        assert_eq!(
            expand_home_with("~\\logs", Some(home), true),
            home.join("logs")
        );
        assert_eq!(
            expand_home_with("~\\logs", Some(home), false),
            PathBuf::from("~\\logs")
        );
        assert_eq!(expand_home_with("~", None, true), PathBuf::from("~"));
        assert_eq!(
            expand_home_with("~other/logs", Some(home), false),
            PathBuf::from("~other/logs")
        );
    }

    #[test]
    fn windows_and_unix_executables_keep_profile_identity() {
        for name in [
            "usagestat-dev",
            "usagestatd-dev",
            "usagestat-dev.exe",
            "usagestatd-dev.exe",
        ] {
            assert_eq!(profile_name(Some(Path::new(name))), "usagestat-dev");
        }
        for name in ["usagestat", "usagestatd.exe", "unrelated-dev.exe"] {
            assert_eq!(profile_name(Some(Path::new(name))), "usagestat");
        }
        assert_eq!(profile_name(None), "usagestat");
    }

    #[test]
    fn overrides_are_complete_paths_and_empty_overrides_use_native_defaults() {
        let native = PathBuf::from("native settings");
        let custom = PathBuf::from("redirected/使用 settings");
        assert_eq!(
            app_path(
                Some(custom.clone().into_os_string()),
                Some(native.clone()),
                "usagestat-dev",
                "configuration",
                "USAGESTAT_CONFIG_DIR"
            )
            .unwrap(),
            custom
        );
        assert_eq!(
            app_path(
                Some(OsString::new()),
                Some(native.clone()),
                "usagestat-dev",
                "configuration",
                "USAGESTAT_CONFIG_DIR"
            )
            .unwrap(),
            native.join("usagestat-dev")
        );
        assert_eq!(
            app_path(
                None,
                Some(native.clone()),
                "usagestat",
                "data",
                "USAGESTAT_DATA_DIR"
            )
            .unwrap(),
            native.join("usagestat")
        );
    }

    #[test]
    fn missing_native_directories_fail_with_an_actionable_override() {
        let error = app_path(None, None, "usagestat", "data", "USAGESTAT_DATA_DIR").unwrap_err();
        assert!(error.to_string().contains("USAGESTAT_DATA_DIR"));
        assert!(
            app_path(
                Some(OsString::new()),
                None,
                "usagestat",
                "data",
                "USAGESTAT_DATA_DIR"
            )
            .is_err()
        );
        assert_eq!(
            app_path(
                Some("explicit 使用/state".into()),
                None,
                "usagestat",
                "data",
                "USAGESTAT_DATA_DIR"
            )
            .unwrap(),
            PathBuf::from("explicit 使用/state")
        );
    }

    #[test]
    fn installed_resources_cover_prefix_archive_npm_and_app_layouts() {
        assert_eq!(
            installed_plugin_dirs(Path::new("prefix/bin/usagestat"), "usagestat"),
            vec![
                PathBuf::from("prefix/share/usagestat/plugins"),
                PathBuf::from("prefix/lib/usagestat/plugins"),
                PathBuf::from("prefix/plugins"),
                PathBuf::from("prefix/bin/plugins"),
            ]
        );
        assert_eq!(
            installed_plugin_dirs(Path::new("archive 使用/usagestat.exe"), "usagestat"),
            vec![PathBuf::from("archive 使用/plugins")]
        );
        assert_eq!(
            installed_plugin_dirs(Path::new("Usage.app/Contents/MacOS/usagestat"), "usagestat"),
            vec![
                PathBuf::from("Usage.app/Contents/Resources/plugins"),
                PathBuf::from("Usage.app/Contents/MacOS/plugins"),
            ]
        );
        assert_eq!(
            installed_plugin_dirs(Path::new("prefix/bin/usagestatd-dev.exe"), "usagestat-dev")[0],
            PathBuf::from("prefix/share/usagestat-dev/plugins")
        );
    }
}
