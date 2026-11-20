//! Provider icons from the AgenticDriver provider-icons library.
//!
//! The release bundles a copy of the library. A running daemon also checks
//! the library's latest stable GitHub release daily and, once it is verified,
//! serves that version instead, so new and refreshed marks arrive without an
//! app update. Each version's catalog and SVG folder are kept together.
use crate::response_json;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use usagestat_core::ProviderSummary;

const BUNDLED_MANIFEST: &str = include_str!("../../../plugins/_provider-icons/manifest.json");
const BUNDLED_VERSION: &str = "0.1.0-alpha.1";
/// Provider IDs whose mark has another name, shared with tools/provider-icons.mjs.
const PROVIDER_ALIASES: &str = include_str!("provider-icon-aliases.json");
const LATEST_RELEASE: &str =
    "https://api.github.com/repos/agenticdriver/provider-icons/releases/latest";
const MAX_SVG_BYTES: usize = 128 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 4_000;
const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;

/// One icon library version: its catalog and, for a download, its folder.
struct Library {
    version: String,
    source: &'static str,
    catalog: Value,
    /// Downloaded SVGs. `None` for the bundled copy, whose files live in the
    /// plugin folders the daemon already registered.
    root: Option<PathBuf>,
}

fn bundled() -> Library {
    Library {
        version: BUNDLED_VERSION.into(),
        source: "bundled",
        catalog: serde_json::from_str(BUNDLED_MANIFEST).expect("bundled icon catalogue JSON"),
        root: None,
    }
}

fn active_slot() -> &'static RwLock<Arc<Library>> {
    static ACTIVE: std::sync::OnceLock<RwLock<Arc<Library>>> = std::sync::OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(Arc::new(bundled())))
}

fn active() -> Arc<Library> {
    active_slot()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn provider_aliases() -> &'static BTreeMap<String, String> {
    static ALIASES: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    ALIASES.get_or_init(|| serde_json::from_str(PROVIDER_ALIASES).expect("provider icon aliases"))
}

pub fn public_catalog() -> Value {
    let library = active();
    let icons: Vec<_> = library.catalog["icons"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, value)| {
            json!({
                "id":id,"name":value["name"],"alternatives":value["alternatives"],
                "monochrome":true,"color":value.get("color").is_some(),
            })
        })
        .collect();
    let mut aliases = library.catalog["aliases"].clone();
    if let Some(aliases) = aliases.as_object_mut() {
        for (provider, mark) in provider_aliases() {
            aliases.entry(provider.clone()).or_insert(json!(mark));
        }
    }
    json!({"icons":icons,"aliases":aliases,"version":library.version,"source":library.source})
}

fn parameter<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, value)| value)
}

fn read_svg(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > MAX_SVG_BYTES as u64 {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .filter(|s| s.len() <= MAX_SVG_BYTES)
}

/// SVG replies never run scripts, even when opened directly on the
/// dashboard's origin, and are never sniffed as another type.
pub fn response_svg(svg: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         Connection: close\r\n\
         Content-Type: image/svg+xml\r\n\
         Content-Security-Policy: sandbox; default-src 'none'; style-src 'unsafe-inline'\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Cache-Control: no-store\r\n\
         Content-Length: {}\r\n\r\n{svg}",
        svg.len()
    )
}

fn valid_svg_name(name: &str) -> bool {
    name.len() <= 128
        && name.ends_with(".svg")
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// Plain vector artwork only: no scripts, event handlers, embedded documents
/// or references outside the file.
fn safe_svg(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let body = lower.trim_start_matches('\u{feff}').trim_start();
    let body = match body.strip_prefix("<?xml") {
        Some(rest) => rest.split_once("?>").map_or("", |(_, rest)| rest.trim_start()),
        None => body,
    };
    // A plain DOCTYPE naming the SVG DTD is harmless; one that declares
    // entities (an internal subset in brackets) is not.
    let body = match body.strip_prefix("<!doctype") {
        Some(rest) => match rest.split_once('>') {
            Some((declaration, rest)) if !declaration.contains('[') => rest.trim_start(),
            _ => return false,
        },
        None => body,
    };
    if !body.starts_with("<svg") {
        return false;
    }
    let banned = [
        "<script", "javascript:", "<foreignobject", "<iframe", "<embed", "<object", "@import",
        "<!entity",
    ];
    if banned.iter().any(|needle| lower.contains(needle)) {
        return false;
    }
    let bytes = lower.as_bytes();
    // Event-handler attributes such as onload= or onclick =.
    for (index, _) in lower.match_indices("on") {
        let before = index.checked_sub(1).map(|i| bytes[i]);
        if !before.is_some_and(|b| b.is_ascii_whitespace() || b == b'"' || b == b'\'' || b == b'/') {
            continue;
        }
        let rest = &lower[index + 2..];
        let name_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        if name_len > 0 && rest[name_len..].trim_start().starts_with('=') {
            return false;
        }
    }
    // Only same-document references.
    for attribute in ["href=\"", "href='"] {
        for (index, _) in lower.match_indices(attribute) {
            if !lower[index + attribute.len()..].starts_with('#') {
                return false;
            }
        }
    }
    for (index, _) in lower.match_indices("url(") {
        let target = lower[index + 4..].trim_start_matches(['"', '\'', ' ']);
        if !target.starts_with('#') {
            return false;
        }
    }
    true
}

/// A validated library version read from a release archive, in memory.
#[derive(Debug)]
struct Unpacked {
    manifest: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
}

fn unpack(archive: &[u8]) -> anyhow::Result<Unpacked> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut manifest = None;
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for (count, entry) in tar.entries()?.enumerate() {
        if count >= MAX_ENTRIES {
            anyhow::bail!("icon archive has too many entries");
        }
        let mut entry = entry?;
        // Links and directories are never followed or created.
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(path) = path.strip_prefix("package/") else {
            continue;
        };
        let keep = path == "manifest.json"
            || path == "LICENSE"
            || path == "NOTICE"
            || path.strip_prefix("assets/").is_some_and(valid_svg_name)
            || path
                .strip_prefix("licenses/")
                .is_some_and(|name| !name.contains('/') && !name.contains("..") && !name.is_empty());
        if !keep {
            continue;
        }
        total += entry.size();
        if total > MAX_UNPACKED_BYTES {
            anyhow::bail!("icon archive is too large when unpacked");
        }
        let mut bytes = Vec::new();
        (&mut entry).take(MAX_UNPACKED_BYTES).read_to_end(&mut bytes)?;
        if path == "manifest.json" {
            manifest = Some(bytes);
        } else {
            files.insert(path.to_string(), bytes);
        }
    }
    let manifest = manifest.ok_or_else(|| anyhow::anyhow!("icon archive has no manifest"))?;
    let unpacked = Unpacked { manifest, files };
    validate(&unpacked)?;
    Ok(unpacked)
}

/// Every SVG is safe and every file or alias the catalog names exists.
fn validate(unpacked: &Unpacked) -> anyhow::Result<Value> {
    let catalog: Value = serde_json::from_slice(&unpacked.manifest)?;
    if catalog["version"] != 1 {
        anyhow::bail!("unsupported icon catalog version");
    }
    let icons = catalog["icons"]
        .as_object()
        .filter(|icons| !icons.is_empty())
        .ok_or_else(|| anyhow::anyhow!("icon catalog has no icons"))?;
    for (name, bytes) in &unpacked.files {
        if name.starts_with("assets/") {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| anyhow::anyhow!("{name} is not UTF-8"))?;
            if bytes.len() > MAX_SVG_BYTES || !safe_svg(text) {
                anyhow::bail!("{name} is not a plain SVG");
            }
        }
    }
    for (id, icon) in icons {
        for style in ["monochrome", "color"] {
            match icon.get(style) {
                None if style == "color" => {}
                Some(Value::String(file))
                    if valid_svg_name(file) && unpacked.files.contains_key(&format!("assets/{file}")) => {}
                _ => anyhow::bail!("icon {id} names a missing {style} file"),
            }
        }
    }
    if let Some(aliases) = catalog["aliases"].as_object() {
        if let Some((alias, _)) = aliases
            .iter()
            .find(|(_, target)| target.as_str().is_none_or(|t| !icons.contains_key(t)))
        {
            anyhow::bail!("icon alias {alias} has no mark");
        }
    }
    Ok(catalog)
}

fn libraries_dir() -> anyhow::Result<PathBuf> {
    Ok(usagestat_core::paths::data_dir()?.join("provider-icons"))
}

fn valid_tag(tag: &str) -> bool {
    let Some(version) = tag.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Numeric version, for "is this newer". Pre-releases sort before releases.
fn version_key(version: &str) -> (Vec<u64>, bool) {
    let version = version.trim_start_matches('v');
    let (core, pre) = version.split_once('-').map_or((version, None), |(c, p)| (c, Some(p)));
    (
        core.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
        pre.is_none(),
    )
}

/// Loads a saved downloaded version. A missing or damaged one keeps the bundled icons.
fn load_saved(dir: &Path) -> Option<Library> {
    let tag = std::fs::read_to_string(dir.join("current")).ok()?;
    let tag = tag.trim();
    if !valid_tag(tag) {
        return None;
    }
    let root = dir.join(tag);
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(root.join("assets")).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if valid_svg_name(&name) {
            files.insert(format!("assets/{name}"), std::fs::read(entry.path()).ok()?);
        }
    }
    let unpacked = Unpacked {
        manifest: std::fs::read(root.join("manifest.json")).ok()?,
        files,
    };
    let catalog = validate(&unpacked)
        .inspect_err(|error| log::warn!("saved provider icons {tag} ignored: {error}"))
        .ok()?;
    Some(Library {
        version: tag.trim_start_matches('v').into(),
        source: "downloaded",
        catalog,
        root: Some(root),
    })
}

/// Uses a saved downloaded library when it is newer than the bundled one.
pub fn load() {
    let Ok(dir) = libraries_dir() else { return };
    if let Some(library) = load_saved(&dir)
        .filter(|library| version_key(&library.version) > version_key(BUNDLED_VERSION))
    {
        *active_slot().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(library);
    }
}

/// Writes a verified version next to the others, then switches the pointer.
fn install(dir: &Path, tag: &str, unpacked: &Unpacked) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let staging = dir.join(format!(".{tag}.partial"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(staging.join("assets"))?;
    std::fs::create_dir_all(staging.join("licenses"))?;
    std::fs::write(staging.join("manifest.json"), &unpacked.manifest)?;
    for (name, bytes) in &unpacked.files {
        std::fs::write(staging.join(name), bytes)?;
    }
    let target = dir.join(tag);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target)?;
    let previous = std::fs::read_to_string(dir.join("current")).unwrap_or_default();
    usagestat_core::storage::write_atomic(&dir.join("current"), tag.as_bytes())?;
    // Keep the active and previous versions only.
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if valid_tag(&name) && name != tag && name != previous.trim() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    Ok(())
}

fn client() -> anyhow::Result<reqwest::blocking::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Release assets redirect to GitHub's download host; nowhere else.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let host = attempt.url().host_str().unwrap_or_default().to_string();
        let github = host == "github.com"
            || host.ends_with(".github.com")
            || host.ends_with(".githubusercontent.com");
        if attempt.url().scheme() == "https" && github && attempt.previous().len() < 5 {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });
    Ok(reqwest::blocking::Client::builder()
        .redirect(policy)
        .https_only(true)
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("usagestat/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Checks the latest stable release and installs it when it is newer than the
/// active library. Returns true when a new version became active.
fn update() -> anyhow::Result<bool> {
    let client = client()?;
    let release: Value = client
        .get(LATEST_RELEASE)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .json()?;
    let tag = release["tag_name"].as_str().unwrap_or_default();
    if !valid_tag(tag) {
        anyhow::bail!("provider icons release has an unexpected tag");
    }
    if version_key(tag) <= version_key(&active().version) {
        return Ok(false);
    }
    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let archives: Vec<_> = assets
        .iter()
        .filter(|asset| {
            asset["name"].as_str().is_some_and(|name| {
                name.starts_with("agenticdriver-provider-icons-") && name.ends_with(".tgz")
            })
        })
        .collect();
    let [archive] = archives.as_slice() else {
        anyhow::bail!("provider icons release needs exactly one archive");
    };
    let name = archive["name"].as_str().unwrap_or_default();
    let url = archive["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("provider icons archive has no download URL"))?;
    let expected = match archive["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")) {
        Some(digest) => digest.to_ascii_lowercase(),
        None => checksum_from_sums(&client, &assets, name)?,
    };
    let mut bytes = Vec::new();
    client
        .get(url)
        .send()?
        .error_for_status()?
        .take(MAX_ARCHIVE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        anyhow::bail!("provider icons archive is too large");
    }
    if sha256_hex(&bytes) != expected {
        anyhow::bail!("provider icons archive does not match its published SHA-256");
    }
    let unpacked = unpack(&bytes)?;
    let dir = libraries_dir()?;
    install(&dir, tag, &unpacked)?;
    let library = load_saved(&dir).ok_or_else(|| anyhow::anyhow!("installed icons did not load"))?;
    *active_slot().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(library);
    Ok(true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn checksum_from_sums(
    client: &reqwest::blocking::Client,
    assets: &[Value],
    name: &str,
) -> anyhow::Result<String> {
    let url = assets
        .iter()
        .find(|asset| asset["name"] == "SHA256SUMS")
        .and_then(|asset| asset["browser_download_url"].as_str())
        .ok_or_else(|| anyhow::anyhow!("provider icons release has no checksum"))?;
    let mut text = String::new();
    client
        .get(url)
        .send()?
        .error_for_status()?
        .take(64 * 1024)
        .read_to_string(&mut text)?;
    text.lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, file)| file.trim().trim_start_matches('*') == name)
        .map(|(sum, _)| sum.to_ascii_lowercase())
        .filter(|sum| sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow::anyhow!("provider icons checksum is missing"))
}

/// Checks for a newer icon library a minute after start and then daily.
pub fn start_updates(shutdown: Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    std::thread::spawn(move || {
        let mut next = Instant::now() + Duration::from_secs(60);
        while !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            if Instant::now() < next {
                continue;
            }
            next = Instant::now() + Duration::from_secs(24 * 60 * 60);
            match update() {
                Ok(true) => log::info!("installed provider icons {}", active().version),
                Ok(false) => {}
                Err(error) => log::warn!("provider icons update failed: {error}"),
            }
        }
    });
}

pub fn serve(id: &str, query: &str, providers: &[ProviderSummary]) -> Option<String> {
    let style = parameter(query, "style");
    let source = parameter(query, "source");
    if style.is_none() && source.is_none() {
        return None;
    }
    let style = style.unwrap_or("color");
    if !["monochrome", "color"].contains(&style) {
        return Some(response_json(
            400,
            "Bad Request",
            r#"{"error":"invalid_icon_style"}"#,
        ));
    }
    let library = active();
    let provider = providers.iter().find(|p| p.id == id);
    let chosen =
        source.unwrap_or_else(|| provider.and_then(|p| p.plugin_id.as_deref()).unwrap_or(id));
    // An explicit choice names a mark; a provider ID may need the shared table.
    let chosen = match source {
        Some(_) => chosen,
        None => provider_aliases().get(chosen).map_or(chosen, String::as_str),
    };
    let chosen = library.catalog["aliases"][chosen].as_str().unwrap_or(chosen);
    let entry = &library.catalog["icons"][chosen];
    if let Some(file) = entry
        .get(style)
        .or_else(|| entry.get("monochrome"))
        .and_then(Value::as_str)
        .filter(|file| valid_svg_name(file))
    {
        if let Some(root) = &library.root {
            if let Some(svg) = read_svg(&root.join("assets").join(file)) {
                return Some(response_svg(&svg));
            }
        }
        for provider in providers {
            if let Some(icon) = &provider.icon {
                for path in [&icon.path, &icon.monochrome_path, &icon.color_path]
                    .into_iter()
                    .flatten()
                {
                    let Some(parent) = Path::new(path).parent() else {
                        continue;
                    };
                    let root = if parent.file_name().is_some_and(|n| n == "_provider-icons") {
                        parent.to_path_buf()
                    } else {
                        parent.parent().unwrap_or(parent).join("_provider-icons")
                    };
                    if let Some(svg) = read_svg(&root.join(file)) {
                        return Some(response_svg(&svg));
                    }
                }
            }
        }
    }
    // Providers outside the shared catalogue can still choose the monochrome
    // and colour variants declared by their own plugin manifest.
    if source.is_none() {
        if let Some(icon) = provider.and_then(|p| p.icon.as_ref()) {
            let file = if style == "monochrome" {
                icon.monochrome_path.as_ref().or(icon.path.as_ref())
            } else {
                icon.color_path.as_ref().or(icon.path.as_ref())
            };
            if let Some(svg) = file.and_then(|file| read_svg(Path::new(file))) {
                return Some(response_svg(&svg));
            }
        }
    }
    Some(response_json(
        404,
        "Not Found",
        r#"{"error":"icon_not_found"}"#,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svg(body: &str) -> Vec<u8> {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">{body}</svg>"#).into_bytes()
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn manifest(icons: &str) -> Vec<u8> {
        format!(r#"{{"version":1,"icons":{{{icons}}},"aliases":{{"chatgpt":"openai"}}}}"#).into_bytes()
    }

    const OPENAI: &str = r#""openai":{"name":"OpenAI","monochrome":"openai.svg","color":"openai-color.svg"}"#;

    fn good() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("package/manifest.json", manifest(OPENAI)),
            ("package/assets/openai.svg", svg(r#"<path d="M0 0h24v24z"/>"#)),
            ("package/assets/openai-color.svg", svg(r##"<defs><linearGradient id="g"/></defs><path fill="url(#g)" d="M0 0"/><use href="#g"/>"##)),
            ("package/LICENSE", b"MIT".to_vec()),
            ("package/react/openai.js", b"export default 1".to_vec()),
        ]
    }

    fn build(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        archive(&entries.iter().map(|(p, b)| (*p, b.as_slice())).collect::<Vec<_>>())
    }

    #[test]
    fn a_plain_svg_doctype_is_accepted() {
        assert!(safe_svg(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n<svg viewBox=\"0 0 1 1\"/>"
        ));
        assert!(!safe_svg("<!DOCTYPE svg [<!ENTITY a \"b\">]><svg/>"));
    }

    #[test]
    fn a_valid_release_keeps_only_catalog_svgs_and_notices() {
        let unpacked = unpack(&build(&good())).unwrap();
        assert_eq!(
            unpacked.files.keys().collect::<Vec<_>>(),
            ["LICENSE", "assets/openai-color.svg", "assets/openai.svg"],
            "JavaScript and other files are never kept"
        );
    }

    /// The good release with one file replaced or added.
    fn with(path: &'static str, bytes: Vec<u8>) -> Vec<u8> {
        let mut entries = good();
        entries.retain(|(p, _)| *p != path);
        entries.push((path, bytes));
        build(&entries)
    }

    #[test]
    fn unsafe_or_incomplete_releases_are_rejected_whole() {
        let icon = "package/assets/openai.svg";
        let cases = [
            ("script", with(icon, svg("<script>alert(1)</script>"))),
            ("handler", with(icon, svg(r#"<path onload ="x()"/>"#))),
            ("external link", with(icon, svg(r#"<use href="https://x/y.svg#a"/>"#))),
            ("external url", with(icon, svg(r#"<path style="fill:url(https://x)"/>"#))),
            ("foreign object", with(icon, svg("<foreignObject/>"))),
            ("entity", with(icon, b"<!DOCTYPE svg [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><svg/>".to_vec())),
            ("not svg", with(icon, b"<html></html>".to_vec())),
            ("too big", with(icon, svg(&"<g/>".repeat(40_000)))),
            ("missing file", with("package/manifest.json", manifest(&OPENAI.replace("openai-color", "absent")))),
            ("dangling alias", with("package/manifest.json",
                br#"{"version":1,"icons":{},"aliases":{"chatgpt":"openai"}}"#.to_vec())),
            ("no manifest", build(&good()[1..])),
        ];
        for (name, bytes) in cases {
            assert!(unpack(&bytes).is_err(), "{name} must be rejected");
        }
        let mut many = good();
        let filler = svg("");
        for index in 0..MAX_ENTRIES {
            many.push((Box::leak(format!("package/extra/{index}").into_boxed_str()), filler.clone()));
        }
        assert!(unpack(&build(&many)).is_err(), "entry cap");
    }

    #[test]
    fn traversal_and_links_are_never_extracted() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, bytes) in good() {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, path, bytes.as_slice()).unwrap();
        }
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        builder.append_link(&mut link, "package/assets/evil.svg", "/etc/passwd").unwrap();
        let mut raw = tar::Header::new_old();
        let name = b"package/assets/../../escape.svg";
        raw.as_old_mut().name[..name.len()].copy_from_slice(name);
        raw.set_size(4);
        raw.set_mode(0o644);
        raw.set_cksum();
        builder.append(&raw, &b"<svg"[..]).unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        let unpacked = unpack(&bytes).unwrap();
        assert!(unpacked.files.keys().all(|name| !name.contains("evil") && !name.contains("..")));
    }

    #[test]
    fn installed_versions_load_and_a_broken_pointer_keeps_bundled_icons() {
        let dir = tempfile::tempdir().unwrap();
        let unpacked = unpack(&build(&good())).unwrap();
        install(dir.path(), "v1.1.0", &unpacked).unwrap();
        let library = load_saved(dir.path()).unwrap();
        assert_eq!((library.version.as_str(), library.source), ("1.1.0", "downloaded"));
        assert!(library.root.unwrap().join("assets/openai.svg").exists());
        install(dir.path(), "v1.2.0", &unpacked).unwrap();
        install(dir.path(), "v1.3.0", &unpacked).unwrap();
        assert!(!dir.path().join("v1.1.0").exists(), "only the active and previous versions stay");
        assert!(dir.path().join("v1.2.0").exists());
        std::fs::write(dir.path().join("current"), "v9.9.9").unwrap();
        assert!(load_saved(dir.path()).is_none());
        std::fs::write(dir.path().join("current"), "../escape").unwrap();
        assert!(load_saved(dir.path()).is_none());
    }

    #[test]
    fn versions_compare_numerically_and_tags_are_plain() {
        assert!(version_key("v1.10.0") > version_key("1.9.3"));
        assert!(version_key("1.1.0") > version_key(BUNDLED_VERSION));
        assert!(version_key("0.1.0") > version_key("0.1.0-alpha.7"));
        assert!(valid_tag("v1.1.0"));
        for tag in ["1.1.0", "v1.1", "v1.1.0-beta", "v1.1.0/../x", "v1..0"] {
            assert!(!valid_tag(tag), "{tag}");
        }
    }

    #[test]
    fn svg_replies_cannot_run_scripts_and_provider_ids_reach_renamed_marks() {
        let reply = response_svg("<svg/>");
        assert!(reply.contains("Content-Security-Policy: sandbox; default-src 'none'"));
        assert!(reply.contains("X-Content-Type-Options: nosniff"));
        assert_eq!(provider_aliases()["typesafe"], "typesafeai");
        let catalog = public_catalog();
        assert_eq!(catalog["source"], "bundled");
        assert_eq!(catalog["aliases"]["abacus-ai"], "abacus");
    }
}
