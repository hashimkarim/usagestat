//! Upstream application roots. Explicit profiles never fall back to another account.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ProviderPathError {
    #[error("native home directory unavailable; set {0} to the provider profile directory")]
    MissingHome(&'static str),
    #[error("CODEX_HOME must name an existing accessible directory")]
    InvalidCodexHome,
}

/// Electron/VS Code user data: APPDATA (native roaming Known Folder fallback)
/// on Windows, Application Support on macOS, and XDG config on Linux.
pub fn app_support_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(value) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        return path.is_absolute().then_some(path);
    }
    dirs::config_dir()
}

pub fn app_support_path(relative: &str) -> Option<PathBuf> {
    append_suffix(app_support_dir(), relative)
}

/// Native local app data. Honor an absolute LOCALAPPDATA override on Windows,
/// using Known Folders when absent; other platforms use dirs' native data root.
pub fn local_app_data_path(relative: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(value) = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        return append_suffix(path.is_absolute().then_some(path), relative);
    }
    append_suffix(dirs::data_local_dir(), relative)
}

fn append_suffix(root: Option<PathBuf>, relative: &str) -> Option<PathBuf> {
    // Plugins pass portable slash-separated suffixes, never another root.
    if relative.is_empty()
        || relative.contains(['\\', ':', '\0'])
        || relative
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return None;
    }
    root.map(|root| root.join(relative))
}

pub fn claude_usage_roots() -> Result<Vec<PathBuf>, ProviderPathError> {
    claude_roots(
        crate::paths::home_dir().as_deref(),
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        cfg!(target_os = "macos"),
    )
}

fn claude_roots(
    home: Option<&Path>,
    explicit: Option<OsString>,
    macos: bool,
) -> Result<Vec<PathBuf>, ProviderPathError> {
    if let Some(explicit) = explicit.filter(|value| !value.is_empty()) {
        return Ok(vec![PathBuf::from(explicit).join("projects")]);
    }
    let home = home.ok_or(ProviderPathError::MissingHome("CLAUDE_CONFIG_DIR"))?;
    let mut roots = vec![home.join(".claude/projects")];
    if macos {
        roots.push(home.join("Library/Developer/Xcode/CodingAssistant/ClaudeAgentConfig/projects"));
    }
    Ok(roots)
}

pub fn codex_home() -> Result<PathBuf, ProviderPathError> {
    codex_root(
        crate::paths::home_dir().as_deref(),
        std::env::var_os("CODEX_HOME"),
    )
}

fn codex_root(
    home: Option<&Path>,
    explicit: Option<OsString>,
) -> Result<PathBuf, ProviderPathError> {
    if let Some(explicit) = explicit.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(explicit);
        if !path.is_dir() {
            return Err(ProviderPathError::InvalidCodexHome);
        }
        return path
            .canonicalize()
            .map_err(|_| ProviderPathError::InvalidCodexHome);
    }
    Ok(home
        .ok_or(ProviderPathError::MissingHome("CODEX_HOME"))?
        .join(".codex"))
}

pub fn codex_usage_roots() -> Result<Vec<PathBuf>, ProviderPathError> {
    let root = codex_home()?;
    Ok(vec![root.join("sessions"), root.join("archived_sessions")])
}

/// Gemini CLI chat roots (`<root>/<project>/chats`). `GEMINI_DATA_DIR` names
/// one or more comma-separated `tmp` roots and is authoritative; otherwise the
/// CLI's own `GEMINI_CLI_HOME` relocation is honored before the native home.
pub fn gemini_usage_roots() -> Result<Vec<PathBuf>, ProviderPathError> {
    gemini_roots(
        crate::paths::home_dir().as_deref(),
        std::env::var_os("GEMINI_DATA_DIR"),
        std::env::var_os("GEMINI_CLI_HOME"),
    )
}

fn gemini_roots(
    home: Option<&Path>,
    explicit: Option<OsString>,
    cli_home: Option<OsString>,
) -> Result<Vec<PathBuf>, ProviderPathError> {
    if let Some(roots) = explicit_roots(explicit) {
        return Ok(roots);
    }
    if let Some(cli_home) = cli_home.filter(|value| !value.is_empty()) {
        return Ok(vec![PathBuf::from(cli_home).join(".gemini/tmp")]);
    }
    Ok(vec![
        home.ok_or(ProviderPathError::MissingHome("GEMINI_DATA_DIR"))?
            .join(".gemini/tmp"),
    ])
}

/// Amp thread roots (`<root>/threads`). `AMP_DATA_DIR` names one or more
/// comma-separated data directories and is authoritative; otherwise Amp's
/// XDG data directory is used on every platform.
pub fn amp_usage_roots() -> Result<Vec<PathBuf>, ProviderPathError> {
    amp_roots(
        crate::paths::home_dir().as_deref(),
        std::env::var_os("AMP_DATA_DIR"),
        std::env::var_os("XDG_DATA_HOME"),
    )
}

fn amp_roots(
    home: Option<&Path>,
    explicit: Option<OsString>,
    xdg_data: Option<OsString>,
) -> Result<Vec<PathBuf>, ProviderPathError> {
    let data = match explicit_roots(explicit) {
        Some(roots) => roots,
        None => match xdg_data
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            Some(xdg) => vec![xdg.join("amp")],
            None => vec![
                home.ok_or(ProviderPathError::MissingHome("AMP_DATA_DIR"))?
                    .join(".local/share/amp"),
            ],
        },
    };
    Ok(data.into_iter().map(|root| root.join("threads")).collect())
}

/// ccusage-compatible comma-separated override. A set but blank value is
/// treated as unset, matching the other provider overrides.
fn explicit_roots(explicit: Option<OsString>) -> Option<Vec<PathBuf>> {
    let explicit = explicit.filter(|value| !value.is_empty())?;
    let explicit = explicit.to_string_lossy();
    let roots: Vec<_> = explicit
        .split(',')
        .map(str::trim)
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .collect();
    (!roots.is_empty()).then_some(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_explicit_profile_excludes_default_and_xcode_even_without_home() {
        let explicit = PathBuf::from("account 使用 with spaces");
        for macos in [false, true] {
            for home in [None, Some(Path::new("other-account"))] {
                assert_eq!(
                    claude_roots(home, Some(explicit.clone().into()), macos).unwrap(),
                    vec![explicit.join("projects")]
                );
            }
        }
        assert!(claude_roots(None, None, false).is_err());
        assert_eq!(
            claude_roots(Some(Path::new("home")), None, false).unwrap(),
            vec![PathBuf::from("home/.claude/projects")]
        );
        assert_eq!(
            claude_roots(Some(Path::new("home")), None, true)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn codex_override_is_authoritative_canonical_and_requires_a_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let profile = fixture.path().join("account 使用 with spaces");
        std::fs::create_dir(&profile).unwrap();
        assert_eq!(
            codex_root(None, Some(profile.clone().into())).unwrap(),
            profile.canonicalize().unwrap()
        );
        assert!(codex_root(Some(fixture.path()), Some(profile.join("missing").into())).is_err());
        let file = fixture.path().join("file");
        std::fs::write(&file, b"fixture").unwrap();
        assert!(codex_root(Some(fixture.path()), Some(file.into())).is_err());
        assert!(codex_root(None, None).is_err());
        assert_eq!(
            codex_root(Some(fixture.path()), Some(OsString::new())).unwrap(),
            fixture.path().join(".codex")
        );
    }

    #[test]
    fn gemini_override_lists_are_authoritative_before_cli_home_and_home() {
        let roots = gemini_roots(
            Some(Path::new("home")),
            Some("first, second ,".into()),
            Some("cli".into()),
        )
        .unwrap();
        assert_eq!(roots, vec![PathBuf::from("first"), PathBuf::from("second")]);
        assert_eq!(
            gemini_roots(None, Some("only".into()), None).unwrap(),
            vec![PathBuf::from("only")]
        );
        assert_eq!(
            gemini_roots(Some(Path::new("home")), Some("".into()), Some("cli".into())).unwrap(),
            vec![PathBuf::from("cli/.gemini/tmp")]
        );
        assert_eq!(
            gemini_roots(Some(Path::new("home")), None, None).unwrap(),
            vec![PathBuf::from("home/.gemini/tmp")]
        );
        assert!(gemini_roots(None, None, None).is_err());
    }

    #[test]
    fn amp_override_excludes_xdg_and_home_and_relative_xdg_is_ignored() {
        let absolute = std::env::temp_dir().join("xdg data");
        assert_eq!(
            amp_roots(None, Some("a,b".into()), Some(absolute.clone().into())).unwrap(),
            vec![PathBuf::from("a/threads"), PathBuf::from("b/threads")]
        );
        assert_eq!(
            amp_roots(Some(Path::new("home")), None, Some(absolute.clone().into())).unwrap(),
            vec![absolute.join("amp/threads")]
        );
        assert_eq!(
            amp_roots(Some(Path::new("home")), None, Some("relative".into())).unwrap(),
            vec![PathBuf::from("home/.local/share/amp/threads")]
        );
        assert!(amp_roots(None, None, None).is_err());
    }
}
