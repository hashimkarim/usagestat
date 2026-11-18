use super::*;
use usagestat_core::provider_paths;

const BROWSERS: [&str; 3] = ["chrome", "brave", "chromium"];
fn suffix(platform: Platform, browser: &str) -> &'static str {
    match (platform, browser) {
        (Platform::Linux, "chrome") => "google-chrome",
        (Platform::Linux, "brave") => "BraveSoftware/Brave-Browser",
        (Platform::Linux, _) => "chromium",
        (Platform::Macos, "chrome") => "Google/Chrome",
        (Platform::Macos, "brave") => "BraveSoftware/Brave-Browser",
        (Platform::Macos, _) => "Chromium",
        (Platform::Windows, "chrome") => "Google/Chrome/User Data",
        (Platform::Windows, "brave") => "BraveSoftware/Brave-Browser/User Data",
        (Platform::Windows, _) => "Chromium/User Data",
    }
}
pub(super) fn discover(platform: Platform, options: &ImportOptions) -> Result<Vec<Profile>> {
    discover_with(platform, options, |browser| {
        if platform == Platform::Windows {
            provider_paths::local_app_data_path(suffix(platform, browser))
        } else {
            provider_paths::app_support_path(suffix(platform, browser))
        }
    })
}
pub(super) fn discover_with(
    platform: Platform,
    options: &ImportOptions,
    root: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Vec<Profile>> {
    if options
        .browser
        .as_deref()
        .is_some_and(|b| !BROWSERS.contains(&b))
    {
        return Err(error(
            "BROWSER_UNSUPPORTED",
            "Select chrome, brave or chromium; use manual credentials for other browser families.",
        ));
    }
    if let Some(directory) = &options.user_data_dir {
        if options.browser.is_none() || !directory.is_absolute() {
            return Err(error(
                "INVALID_PROFILE",
                "--user-data-dir requires --browser and an absolute browser data directory.",
            ));
        }
    }
    if options.profile.as_deref().is_some_and(|p| {
        p.is_empty() || p == "." || p == ".." || p.contains(['/', '\\', ':', '\0'])
    }) {
        return Err(error(
            "INVALID_PROFILE",
            "--profile must be a single browser profile directory name.",
        ));
    }
    let mut profiles = Vec::new();
    let mut found_browser = false;
    for browser in BROWSERS {
        if options.browser.as_deref().is_some_and(|b| b != browser) {
            continue;
        }
        let Some(directory) = options.user_data_dir.clone().or_else(|| root(browser)) else {
            continue;
        };
        if !directory.is_dir() {
            continue;
        }
        found_browser = true;
        let names = if let Some(profile) = &options.profile {
            vec![profile.clone()]
        } else {
            let mut names = vec!["Default".to_owned()];
            let entries = std::fs::read_dir(&directory).map_err(|_| {
                error(
                    "COOKIE_DB_UNAVAILABLE",
                    "Browser profile directory is inaccessible.",
                )
            })?;
            for entry in entries {
                let entry = entry.map_err(|_| {
                    error(
                        "COOKIE_DB_UNAVAILABLE",
                        "Browser profile directory is inaccessible.",
                    )
                })?;
                let name = entry.file_name();
                if let Some(name) = name.to_str().filter(|name| name.starts_with("Profile ")) {
                    names.push(name.to_owned());
                }
                if names.len() > 256 {
                    return Err(error(
                        "INVALID_PROFILE",
                        "Too many browser profiles; select --profile.",
                    ));
                }
            }
            names.sort();
            names.dedup();
            names
        };
        for name in names {
            let profile = directory.join(&name);
            let modern = profile.join("Network/Cookies");
            let legacy = profile.join("Cookies");
            let db = if modern.is_file() {
                modern
            } else if legacy.is_file() {
                legacy
            } else {
                continue;
            };
            profiles.push(Profile {
                browser,
                name,
                db,
                platform,
                data_dir: directory.clone(),
                secret_app_ids: match browser {
                    "chrome" => &["chrome", "google-chrome"],
                    "brave" => &["brave", "brave-browser"],
                    _ => &["chromium"],
                },
                mac_keychain: match browser {
                    "chrome" => Some(("Chrome Safe Storage", "Chrome")),
                    "chromium" => Some(("Chromium Safe Storage", "Chromium")),
                    "brave" => Some(("Brave Safe Storage", "Brave")),
                    _ => None,
                },
            });
        }
    }
    if profiles.is_empty() {
        return Err(error(
            if found_browser {
                "PROFILE_NOT_FOUND"
            } else {
                "BROWSER_NOT_FOUND"
            },
            "No selected browser cookie database exists. Sign in, check the profile selection, or use manual credentials.",
        ));
    }
    Ok(profiles)
}
