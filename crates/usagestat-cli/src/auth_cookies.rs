//! Scoped Chromium cookie import. Read-only SQLite transactions provide a live
//! snapshot without copying browser credentials to temporary databases.
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod crypto;
mod profiles;
#[cfg(test)]
mod tests;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieImportResult {
    pub provider_id: String,
    pub cookie_header: String,
    pub source: String,
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_full_curl_on_challenge: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_curl_instructions: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CookieImportError {
    pub error: String,
    pub message: String,
}
type Result<T> = std::result::Result<T, CookieImportError>;
fn error(code: &str, message: &str) -> CookieImportError {
    CookieImportError {
        error: code.into(),
        message: message.into(),
    }
}

#[derive(Default)]
pub struct ImportOptions {
    pub browser: Option<String>,
    pub profile: Option<String>,
    pub user_data_dir: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Platform {
    Linux,
    Macos,
    Windows,
}
impl Platform {
    fn current() -> Result<Self> {
        if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Ok(Self::Macos)
        } else if cfg!(windows) {
            Ok(Self::Windows)
        } else {
            Err(error(
                "PLATFORM_UNSUPPORTED",
                "Use provider-specific manual credentials on this platform.",
            ))
        }
    }
}

#[derive(Clone)]
struct Profile {
    browser: &'static str,
    name: String,
    db: PathBuf,
    #[cfg_attr(not(windows), allow(dead_code))]
    data_dir: PathBuf,
    platform: Platform,
    secret_app_ids: &'static [&'static str],
    mac_keychain: Option<(&'static str, &'static str)>,
}
// Deliberately no Debug: records include authentication material.
struct Cookie {
    name: String,
    value: String,
    encrypted: Vec<u8>,
    domain: String,
    path: String,
}
struct Snapshot {
    version: u32,
    cookies: Vec<Cookie>,
}

pub fn import_cookies(
    provider_id: &str,
    web_url: &str,
    options: &ImportOptions,
) -> Result<CookieImportResult> {
    let platform = Platform::current()?;
    let url = reqwest::Url::parse(web_url)
        .map_err(|_| error("INVALID_WEB_URL", "Provider web URL is invalid."))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(
            "INVALID_WEB_URL",
            "Provider web URL must be HTTP(S), with a host and no credentials.",
        ));
    }
    let profiles = profiles::discover(platform, options)?;
    let mut decoder = crypto::Decoder::default();
    import_selected(provider_id, &url, profiles, |profile, cookie, version| {
        decoder.decode(profile, cookie, version)
    })
}

fn import_selected(
    provider_id: &str,
    url: &reqwest::Url,
    profiles: Vec<Profile>,
    mut decrypt: impl FnMut(&Profile, &Cookie, u32) -> Result<String>,
) -> Result<CookieImportResult> {
    let mut matching = Vec::new();
    let mut read_error = None;
    for profile in profiles {
        match read_snapshot(&profile, url, chromium_now()) {
            Ok(snapshot) if !snapshot.cookies.is_empty() => matching.push((profile, snapshot)),
            Ok(_) => {}
            Err(err) => {
                read_error.get_or_insert(err);
            }
        }
    }
    if let Some(err) = read_error {
        return Err(err);
    }
    // Choose before decrypting: no Keychain prompts or credential combinations
    // from other browser accounts, and no fallback after a selected-store error.
    if matching.len() > 1 {
        return Err(error(
            "AMBIGUOUS_PROFILE",
            "More than one browser profile has matching cookies. Select --browser and --profile, or an explicit --user-data-dir.",
        ));
    }
    let Some((profile, snapshot)) = matching.pop() else {
        return Err(error(
            "SESSION_NOT_FOUND",
            "No unexpired cookies match this provider URL. Sign in or use provider-specific manual credentials.",
        ));
    };
    // Reject known unavailable formats before requesting any native key.
    for cookie in &snapshot.cookies {
        crypto::validate_format(profile.platform, cookie)?;
    }
    let mut cookies = Vec::new();
    for mut cookie in snapshot.cookies {
        cookie.value = decrypt(&profile, &cookie, snapshot.version)?;
        if !valid_name(&cookie.name) || !valid_value(&cookie.value) {
            return Err(error(
                "COOKIE_DECRYPT_FAILED",
                "Cookie data is malformed; use provider-specific manual credentials.",
            ));
        }
        cookies.push(cookie);
    }
    let header = build_header(cookies, url.host_str().unwrap());
    if header.is_empty() {
        return Err(error(
            "SESSION_NOT_FOUND",
            "No usable cookies match this provider URL.",
        ));
    }
    Ok(CookieImportResult {
        provider_id: provider_id.into(),
        cookie_header: header,
        source: profile.browser.into(),
        profile: profile.name,
        requires_full_curl_on_challenge: None,
        full_curl_instructions: None,
    })
}

fn chromium_now() -> i64 {
    const EPOCH: i64 = 11_644_473_600_000_000;
    EPOCH
        + SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros()
            .min(i64::MAX as u128 - EPOCH as u128) as i64
}
fn domain_matches(domain: &str, host: &str) -> bool {
    if let Some(domain) = domain.strip_prefix('.') {
        !domain.is_empty()
            && (host.eq_ignore_ascii_case(domain)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", domain.to_ascii_lowercase())))
    } else {
        host.eq_ignore_ascii_case(domain)
    }
}
fn path_matches(cookie: &str, request: &str) -> bool {
    cookie.starts_with('/')
        && (cookie == request
            || request
                .strip_prefix(cookie)
                .is_some_and(|rest| cookie.ends_with('/') || rest.starts_with('/')))
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 4096
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b))
}
fn valid_value(value: &str) -> bool {
    value.len() <= 64 * 1024
        && value.bytes().all(|b| {
            b == 0x21
                || (0x23..=0x2b).contains(&b)
                || (0x2d..=0x3a).contains(&b)
                || (0x3c..=0x5b).contains(&b)
                || (0x5d..=0x7e).contains(&b)
        })
}

fn read_snapshot(profile: &Profile, url: &reqwest::Url, now: i64) -> Result<Snapshot> {
    let db_error = || {
        error(
            "COOKIE_DB_UNAVAILABLE",
            "Browser cookie database is locked or inaccessible. Close the browser and retry, or use manual credentials.",
        )
    };
    let schema_error = || {
        error(
            "COOKIE_SCHEMA_UNSUPPORTED",
            "Browser cookie schema is unsupported. Use provider-specific manual credentials.",
        )
    };
    let query_error = |err: rusqlite::Error| match err.sqlite_error_code() {
        Some(
            rusqlite::ErrorCode::DatabaseBusy
            | rusqlite::ErrorCode::DatabaseLocked
            | rusqlite::ErrorCode::CannotOpen
            | rusqlite::ErrorCode::SystemIoFailure
            | rusqlite::ErrorCode::PermissionDenied,
        ) => db_error(),
        _ => schema_error(),
    };
    let conn = Connection::open_with_flags(
        &profile.db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| db_error())?;
    conn.busy_timeout(Duration::from_millis(250))
        .map_err(|_| db_error())?;
    conn.execute_batch("PRAGMA query_only=ON; BEGIN")
        .map_err(|_| db_error())?;
    let version: u32 = conn
        .query_row("SELECT value FROM meta WHERE key='version'", [], |r| {
            r.get::<_, String>(0)?
                .parse()
                .map_err(|_| rusqlite::Error::InvalidQuery)
        })
        .map_err(query_error)?;
    if !(1..=24).contains(&version) {
        return Err(schema_error());
    }
    let columns = conn
        .prepare("PRAGMA table_info(cookies)")
        .map_err(|_| schema_error())?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|_| schema_error())?
        .collect::<std::result::Result<HashSet<_>, _>>()
        .map_err(|_| schema_error())?;
    if ![
        "host_key",
        "name",
        "value",
        "encrypted_value",
        "path",
        "expires_utc",
        "is_secure",
    ]
    .iter()
    .all(|column| columns.contains(*column))
    {
        return Err(schema_error());
    }
    // Host/domain matching is exact below; SQL narrows the read without suffix
    // wildcards that would accidentally include an unrelated sibling domain.
    let host = url.host_str().unwrap();
    let mut domains = vec![host.to_owned(), format!(".{host}")];
    let mut parent = host;
    while let Some((_, suffix)) = parent.split_once('.') {
        domains.push(format!(".{suffix}"));
        parent = suffix;
    }
    let placeholders = vec!["?"; domains.len()].join(",");
    let partition = if columns.contains("top_frame_site_key") {
        "AND COALESCE(top_frame_site_key,'')=''"
    } else {
        ""
    };
    let sql = format!(
        "SELECT host_key,name,value,encrypted_value,path,expires_utc,is_secure FROM cookies WHERE lower(host_key) IN ({placeholders}) {partition} LIMIT 4097"
    );
    let mut stmt = conn.prepare(&sql).map_err(|_| schema_error())?;
    let mut rows = stmt
        .query(rusqlite::params_from_iter(domains))
        .map_err(|_| db_error())?;
    let mut cookies = Vec::new();
    let mut bytes = 0;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(|_| db_error())? {
        count += 1;
        let cookie = Cookie {
            domain: row.get(0).map_err(|_| schema_error())?,
            name: row.get(1).map_err(|_| schema_error())?,
            value: row.get(2).map_err(|_| schema_error())?,
            encrypted: row.get(3).map_err(|_| schema_error())?,
            path: row.get(4).map_err(|_| schema_error())?,
        };
        let expiry: i64 = row.get(5).map_err(|_| schema_error())?;
        let secure: bool = row.get(6).map_err(|_| schema_error())?;
        bytes += cookie.encrypted.len() + cookie.value.len() + cookie.name.len();
        if count > 4096 || bytes > 4 * 1024 * 1024 {
            return Err(error(
                "COOKIE_DB_UNAVAILABLE",
                "Matching cookie data exceeds import limits.",
            ));
        }
        if !domain_matches(&cookie.domain, host)
            || !path_matches(&cookie.path, url.path())
            || (secure && url.scheme() != "https")
            || (expiry != 0 && expiry <= now)
        {
            continue;
        }
        if cookie.value.is_empty() && cookie.encrypted.is_empty() {
            continue;
        }
        if !valid_name(&cookie.name) || cookie.encrypted.len() > 64 * 1024 {
            return Err(error(
                "COOKIE_DECRYPT_FAILED",
                "Matching cookie data is malformed.",
            ));
        }
        cookies.push(cookie);
    }
    Ok(Snapshot { version, cookies })
}
fn build_header(mut cookies: Vec<Cookie>, host: &str) -> String {
    cookies.sort_by(|a, b| {
        b.path
            .len()
            .cmp(&a.path.len())
            .then_with(|| {
                (b.domain.trim_start_matches('.') == host)
                    .cmp(&(a.domain.trim_start_matches('.') == host))
            })
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut seen = HashSet::new();
    cookies
        .into_iter()
        .filter(|c| seen.insert(c.name.clone()))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}
