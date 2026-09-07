//! Saved daemon intent and lifecycle, with platform service operations behind adapters.
use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use usagestat_core::daemon_settings::{
    DaemonSettings, Installation, T3Mode as SavedT3Mode, local_url,
};
use usagestat_core::{AppConfig, paths};

#[cfg(any(target_os = "macos", test))]
mod launchd;
#[cfg(test)]
mod lifecycle_tests;
mod service;
#[cfg(any(target_os = "linux", test))]
mod systemd;
#[cfg(windows)]
mod task_scheduler;
use service::{Registration, ServiceManager};

pub(super) fn dev_profile() -> bool {
    paths::is_dev_profile()
}

fn settings_file() -> Result<PathBuf> {
    Ok(paths::config_dir()?.join("daemon.json"))
}

fn load_settings(manager: &dyn ServiceManager) -> Result<DaemonSettings> {
    let stored = DaemonSettings::load(&settings_file()?).context("read daemon settings")?;
    if stored.as_ref().is_some_and(|s| s.installation.is_some()) {
        return Ok(stored.unwrap());
    }
    let migrated = manager.migrate()?;
    Ok(match (stored, migrated) {
        (Some(mut stored), Some(migrated)) => {
            stored.installation = migrated.installation;
            stored
        }
        (Some(stored), None) => stored,
        (None, Some(migrated)) => migrated,
        (None, None) => DaemonSettings::default(),
    })
}

pub(super) fn dashboard_base_url(bind: Option<SocketAddr>) -> Result<String> {
    let settings = if bind.is_none() {
        Some(load_settings(service::native()?.as_ref())?)
    } else {
        None
    };
    let bind = bind
        .or_else(|| settings.and_then(|s| s.installation.map(|i| i.bind)))
        .unwrap_or_else(|| "127.0.0.1:6736".parse().unwrap());
    if bind.port() == 0 {
        bail!("the dashboard requires a fixed, nonzero daemon port");
    }
    Ok(local_url(bind))
}

pub(super) fn check_health(base_url: &str) -> Result<()> {
    let body: serde_json::Value = local_client(Duration::from_secs(1))?
        .get(format!("{base_url}/health"))
        .send()?
        .error_for_status()?
        .json()?;
    if body.get("status").and_then(|v| v.as_str()) != Some("ok") {
        bail!("unexpected daemon health response");
    }
    Ok(())
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start now and automatically at login, preserving saved settings
    Enable {
        #[arg(long, value_name = "PATH")]
        binary: Option<PathBuf>,
        /// Address (defaults to the saved address, then 127.0.0.1:6736)
        #[arg(long)]
        bind: Option<SocketAddr>,
        #[arg(long)]
        t3: bool,
        /// Explicitly transfer this profile from a different installation
        #[arg(long)]
        switch_owner: bool,
    },
    /// Stop and disable login startup, or disable only T3 with --t3
    Disable {
        #[arg(long)]
        t3: bool,
    },
    /// Stop and remove this profile's managed login registration; retain settings and credentials
    Unregister,
    /// Start the registered daemon without changing login startup
    Start,
    /// Stop the registered daemon without changing login startup
    Stop,
    /// Change login startup without starting or stopping the registered daemon
    Autostart {
        #[arg(value_enum)]
        mode: AutostartMode,
    },
    /// Toggle T3 while preserving whether the daemon is running
    Toggle {
        #[arg(long, required = true)]
        t3: bool,
    },
    /// Save T3 auto/off; restart only an already-running owned service
    T3 {
        #[arg(value_enum)]
        mode: T3Mode,
    },
    /// Show saved configuration, service state and endpoint health
    Status,
    /// Print the existing management key to paste into T3 Code
    Key,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AutostartMode {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum T3Mode {
    Auto,
    Off,
}
impl From<T3Mode> for SavedT3Mode {
    fn from(value: T3Mode) -> Self {
        match value {
            T3Mode::Auto => Self::Auto,
            T3Mode::Off => Self::Off,
        }
    }
}
impl From<SavedT3Mode> for T3Mode {
    fn from(value: SavedT3Mode) -> Self {
        match value {
            SavedT3Mode::Auto => Self::Auto,
            SavedT3Mode::Off => Self::Off,
        }
    }
}
impl T3Mode {
    fn status_text(self, running: bool, available: bool) -> &'static str {
        match (self, running, available) {
            (Self::Off, _, _) => "off",
            (Self::Auto, false, _) => "auto · unavailable (daemon stopped)",
            (Self::Auto, true, true) => "auto · available",
            (Self::Auto, true, false) => "auto · unavailable (bridge not responding)",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatus {
    service: String,
    manager: &'static str,
    manager_available: bool,
    configured: bool,
    registered: bool,
    autostart: bool,
    running: bool,
    healthy: bool,
    condition: &'static str,
    unit_file: Option<PathBuf>,
    owner: Option<PathBuf>,
    backend_version: Option<String>,
    diagnostics: Vec<String>,
    t3_mode: T3Mode,
    t3_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    management_key_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dashboard_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hub_url: Option<String>,
}

pub fn run(
    command: &DaemonCommand,
    config: Option<&Path>,
    extra_dirs: &[PathBuf],
    json: bool,
) -> Result<()> {
    if matches!(command, DaemonCommand::Key) {
        let key_path = DaemonSettings::load(&settings_file()?)?
            .and_then(|s| s.installation)
            .map(|i| i.management_key_file)
            .unwrap_or(paths::management_key_file()?);
        let key = read_key(&key_path)
            .context("no usable management key; run `usagestat daemon t3 auto` first")?;
        if json {
            println!("{}", serde_json::json!({"managementKey": key}));
        } else {
            println!("{key}");
        }
        return Ok(());
    }
    let manager = service::native()?;
    let status = execute(command, config, extra_dirs, manager.as_ref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!(
            "Startup at login: {}",
            if status.autostart {
                "enabled"
            } else {
                "disabled"
            }
        );
        println!("Daemon: {}", status.condition);
        if let Some(url) = status.dashboard_url {
            println!("Dashboard URL: {url}");
        }
        println!(
            "T3: {}",
            status
                .t3_mode
                .status_text(status.running, status.t3_available)
        );
        if let Some(url) = status.hub_url {
            println!("Hub URL: {url}");
        }
        if let Some(path) = status.management_key_file {
            println!("Management key file: {}", path.display());
        }
        for diagnostic in status.diagnostics {
            println!("{diagnostic}");
        }
    }
    Ok(())
}

fn execute(
    command: &DaemonCommand,
    config: Option<&Path>,
    extra_dirs: &[PathBuf],
    manager: &dyn ServiceManager,
) -> Result<ServiceStatus> {
    let _settings_lock = if matches!(command, DaemonCommand::Status) {
        None
    } else {
        Some(
            usagestat_core::storage::exclusive_lock(
                &paths::config_dir()?.join("daemon-settings.lock"),
            )
            .context("another command is changing this profile's daemon settings")?,
        )
    };
    let mut settings = load_settings(manager)?;
    let key_file = settings
        .installation
        .as_ref()
        .map(|i| i.management_key_file.clone())
        .unwrap_or(paths::management_key_file()?);
    let stored_path = settings_file()?;
    match command {
        DaemonCommand::Enable {
            binary,
            bind,
            t3,
            switch_owner,
        } => {
            let binary = find_binary(binary.as_deref())?;
            let owner = installation_owner(&binary)?;
            manager.validate()?;
            if !switch_owner
                && settings
                    .installation
                    .as_ref()
                    .is_some_and(|old| old.owner != owner)
            {
                bail!(
                    "another installation owns this profile ({}); use --switch-owner to transfer it",
                    settings.installation.as_ref().unwrap().owner.display()
                );
            }
            let bind = bind
                .or_else(|| settings.installation.as_ref().map(|old| old.bind))
                .unwrap_or_else(|| "127.0.0.1:6736".parse().unwrap());
            if bind.port() == 0 {
                bail!("daemon autostart requires a fixed, nonzero port");
            }
            let previous = manager.query()?;
            let health = endpoint(&local_url(bind));
            if health.occupied
                && !(previous.running
                    && settings
                        .installation
                        .as_ref()
                        .is_some_and(|old| old.bind == bind))
            {
                bail!(
                    "daemon address {bind} is occupied; preserve that process and choose another --bind address"
                );
            }
            let config = absolute(
                &config
                    .map(Path::to_owned)
                    .or_else(|| settings.installation.as_ref().map(|old| old.config.clone()))
                    .unwrap_or(paths::config_file()?),
            )?;
            let app = AppConfig::load_optional(&config)?;
            let plugin_dirs = if extra_dirs.is_empty() && settings.installation.is_some() {
                let previous = settings.installation.as_ref().unwrap();
                paths::relocate_installed_plugin_dirs(&previous.plugin_dirs, &previous.binary, &binary, paths::app_dir_name())
            } else {
                let selected = paths::plugin_dirs(&app, extra_dirs)?
                    .iter()
                    .map(|dir| absolute(dir))
                    .collect::<Result<Vec<_>>>()?;
                paths::relocate_installed_plugin_dirs(&selected, &std::env::current_exe()?, &binary, paths::app_dir_name())
            };
            let mut environment = settings
                .installation
                .as_ref()
                .map(|old| old.environment.clone())
                .unwrap_or_default();
            for name in [
                "PATH",
                "HOME",
                "USERPROFILE",
                "APPDATA",
                "LOCALAPPDATA",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "USAGESTAT_HELPER_PATH",
            ] {
                if let Ok(value) = std::env::var(name) {
                    environment.insert(name.to_owned(), value);
                }
            }
            for (name, path) in [
                ("USAGESTAT_CONFIG_DIR", paths::config_dir()?),
                ("USAGESTAT_DATA_DIR", paths::data_dir()?),
            ] {
                environment.insert(
                    name.to_owned(),
                    absolute(&path)?
                        .to_str()
                        .context("service paths must be UTF-8")?
                        .to_owned(),
                );
            }
            capture_provider_roots(&mut environment, |name| std::env::var(name).ok())?;
            if *t3 {
                settings.t3_mode = SavedT3Mode::Auto;
            }
            settings.installation = Some(Installation {
                owner,
                binary,
                bind,
                config,
                plugin_dirs,
                environment,
                management_key_file: key_file.clone(),
                control_key_file: paths::control_key_file()?,
            });
            let install = settings.installation.as_ref().unwrap();
            ensure_key(&install.control_key_file)?;
            if settings.t3_mode == SavedT3Mode::Auto {
                ensure_key(&key_file)?;
            }
            settings.save(&stored_path)?;
            manager.install(install, &stored_path)?;
            manager.enable()?;
            wait_for_installation(install)?;
        }
        DaemonCommand::Disable { t3: true }
        | DaemonCommand::Toggle { .. }
        | DaemonCommand::T3 { .. } => {
            let mode = match command {
                DaemonCommand::T3 { mode } => (*mode).into(),
                DaemonCommand::Toggle { .. } if settings.t3_mode == SavedT3Mode::Off => {
                    SavedT3Mode::Auto
                }
                _ => SavedT3Mode::Off,
            };
            if apply_t3(&mut settings, mode, &stored_path, &key_file, manager)? {
                wait_for_installation(settings.installation.as_ref().unwrap())?;
            }
        }
        DaemonCommand::Disable { t3: false } => {
            manager.validate()?;
            settings.save(&stored_path)?;
            manager.disable()?;
        }
        DaemonCommand::Unregister => unregister(&settings, &stored_path, manager)?,
        DaemonCommand::Start | DaemonCommand::Stop | DaemonCommand::Autostart { .. } => {
            manager.validate()?;
            let install = settings.installation.as_ref().context("no saved daemon installation; use daemon enable first")?;
            let registration = manager.query()?;
            anyhow::ensure!(registration.registered, "no managed login registration; use daemon enable first");
            match command {
                DaemonCommand::Start => {
                    if !registration.running && endpoint(&install.base_url()).occupied {
                        bail!("the saved daemon address is occupied; preserve that process before starting this installation");
                    }
                    manager.start()?;
                    wait_for_installation(install)?;
                }
                DaemonCommand::Stop => manager.stop()?,
                DaemonCommand::Autostart { mode } => manager.set_autostart(*mode == AutostartMode::On)?,
                _ => unreachable!(),
            }
        }
        DaemonCommand::Status => {}
        DaemonCommand::Key => unreachable!(),
    }
    service_status(manager, &settings)
}

fn unregister(settings: &DaemonSettings, stored_path: &Path, manager: &dyn ServiceManager) -> Result<()> {
    manager.validate()?;
    // Retain migrated installation/key paths as well as T3 mode. Removing login
    // registration must not discard credentials or silently transfer ownership.
    settings.save(stored_path)?;
    manager.unregister()
}

fn apply_t3(
    settings: &mut DaemonSettings,
    mode: SavedT3Mode,
    stored_path: &Path,
    key_file: &Path,
    manager: &dyn ServiceManager,
) -> Result<bool> {
    let previous = if settings.installation.is_some() {
        manager.validate()?;
        Some(manager.query()?)
    } else {
        None
    };
    if mode == SavedT3Mode::Auto {
        ensure_key(key_file)?;
    }
    if let Some(install) = &settings.installation {
        ensure_key(&install.control_key_file)?;
    }
    settings.t3_mode = mode;
    settings.save(stored_path)?;
    if let Some(install) = &settings.installation {
        manager.install(install, stored_path)?;
        if previous.is_some_and(|state| state.running) {
            manager.restart()?;
            return Ok(true);
        }
    }
    Ok(false)
}

fn installation_owner(binary: &Path) -> Result<PathBuf> {
    if binary
        .components()
        .any(|c| c.as_os_str() == "_npx" || c.as_os_str() == "_cacache")
    {
        bail!(
            "temporary npm execution cannot own autostart; install globally or use a durable native installation"
        );
    }
    // Homebrew changes the versioned Cellar path during upgrades, while its
    // package directory remains the same installation owner.
    let mut prefix = PathBuf::new();
    let mut cellar = false;
    for component in binary.components() {
        prefix.push(component.as_os_str());
        if cellar {
            return Ok(prefix);
        }
        cellar = component.as_os_str() == "Cellar";
    }
    let directory = binary
        .parent()
        .context("daemon executable has no installation directory")?;
    Ok(if directory.file_name().is_some_and(|name| name == "bin") {
        directory.parent().unwrap_or(directory).to_owned()
    } else {
        directory.to_owned()
    })
}

#[derive(Default)]
struct Endpoint {
    occupied: bool,
    healthy: bool,
    version: Option<String>,
    owner: Option<PathBuf>,
}
fn endpoint(url: &str) -> Endpoint {
    let mut result = Endpoint::default();
    if let Some(address) = url
        .strip_prefix("http://")
        .and_then(|s| s.parse::<SocketAddr>().ok())
    {
        result.occupied = TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok();
    }
    let read = || -> Result<serde_json::Value> {
        Ok(local_client(Duration::from_secs(1))?
            .get(format!("{url}/health"))
            .send()?
            .error_for_status()?
            .json()?)
    };
    if let Ok(body) = read() {
        result.occupied = true;
        result.healthy = body["status"] == "ok" && body["application"] == "usagestat";
        result.version = body["version"].as_str().map(str::to_owned);
        result.owner = body["owner"].as_str().map(PathBuf::from);
    }
    result
}

fn service_status(
    manager: &dyn ServiceManager,
    settings: &DaemonSettings,
) -> Result<ServiceStatus> {
    service_status_with_probe(manager, settings, true)
}

fn service_status_with_probe(
    manager: &dyn ServiceManager,
    settings: &DaemonSettings,
    probe_t3: bool,
) -> Result<ServiceStatus> {
    let mut diagnostics = Vec::new();
    let (registration, manager_available) = match manager.query() {
        Ok(registration) => (registration, manager.available()),
        Err(error) => {
            diagnostics.push(error.to_string());
            (Registration::default(), false)
        }
    };
    let url = settings
        .installation
        .as_ref()
        .map(Installation::base_url)
        .unwrap_or_else(|| "http://127.0.0.1:6736".to_owned());
    let health = endpoint(&url);
    let owned = settings
        .installation
        .as_ref()
        .is_some_and(|install| health.owner.as_ref() == Some(&install.owner));
    let condition = if health.occupied && !health.healthy {
        "port-conflict"
    } else if health.healthy && health.version.as_deref() != Some(env!("CARGO_PKG_VERSION")) {
        "wrong-version"
    } else if health.healthy && !owned {
        "external"
    } else if health.healthy {
        "healthy"
    } else if registration.running {
        "starting"
    } else if !manager_available {
        "manager-unavailable"
    } else if registration.registered {
        "stopped"
    } else {
        "unregistered"
    };
    let running = registration.running || health.healthy;
    let key_file = settings
        .installation
        .as_ref()
        .map(|i| i.management_key_file.clone())
        .unwrap_or(paths::management_key_file()?);
    let t3_available = probe_t3
        && settings.t3_mode == SavedT3Mode::Auto
        && health.healthy
        && quota_endpoint_available(&url, &key_file);
    Ok(ServiceStatus {
        service: manager.name(),
        manager: manager.kind(),
        manager_available,
        configured: settings.installation.is_some(),
        registered: registration.registered,
        autostart: registration.enabled,
        running,
        healthy: health.healthy,
        condition,
        unit_file: manager.file(),
        owner: settings.installation.as_ref().map(|i| i.owner.clone()),
        backend_version: health.version,
        diagnostics,
        t3_mode: settings.t3_mode.into(),
        t3_available,
        management_key_file: (settings.t3_mode == SavedT3Mode::Auto).then_some(key_file),
        dashboard_url: health.healthy.then(|| format!("{url}/dashboard")),
        hub_url: t3_available.then_some(url),
    })
}

/// Public diagnostics contain only fixed state and version fields. In
/// particular, never return service XML, settings/environment or native errors.
pub(super) fn diagnostic_status() -> Result<serde_json::Value> {
    let manager = service::native()?;
    let settings = load_settings(manager.as_ref())?;
    let status = service_status_with_probe(manager.as_ref(), &settings, false)?;
    let code = match status.condition {
        "healthy" => "ready",
        "stopped" | "unregistered" => "service-stopped",
        "wrong-version" => "wrong-version",
        "manager-unavailable" => "service-manager-unavailable",
        "external" => "installation-owner-mismatch",
        _ => "unhealthy",
    };
    Ok(serde_json::json!({
        "code": code, "condition": status.condition, "manager": status.manager,
        "managerAvailable": status.manager_available, "configured": status.configured,
        "registered": status.registered, "autostart": status.autostart,
        "running": status.running, "healthy": status.healthy,
        "versionMatches": status.backend_version.as_deref() == Some(env!("CARGO_PKG_VERSION")),
        "t3": "not-checked",
    }))
}

fn wait_for_installation(installation: &Installation) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let health = endpoint(&installation.base_url());
        if health.healthy
            && health.version.as_deref() == Some(env!("CARGO_PKG_VERSION"))
            && health.owner.as_ref() == Some(&installation.owner)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "owned daemon did not become ready at {}; inspect service status or disable it",
                installation.base_url()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

pub(super) fn find_binary(explicit: Option<&Path>) -> Result<PathBuf> {
    let candidates = if let Some(path) = explicit {
        vec![path.to_path_buf()]
    } else {
        let daemon_name = if dev_profile() {
            "usagestatd-dev"
        } else {
            "usagestatd"
        };
        let daemon_name = format!("{daemon_name}{}", std::env::consts::EXE_SUFFIX);
        let mut candidates = vec![std::env::current_exe()?.with_file_name(&daemon_name)];
        if let Some(path) = std::env::var_os("PATH") {
            candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(&daemon_name)));
        }
        candidates
    };
    for candidate in candidates {
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if candidate.metadata()?.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return candidate
                .canonicalize()
                .context("resolve daemon executable");
        }
    }
    bail!(
        "usagestatd was not found; install it beside usagestat, use --binary PATH, or build with `cargo build -p usagestat-cli -p usagestat-daemon`"
    )
}

fn read_key(path: &Path) -> Result<String> {
    let key = usagestat_core::storage::read_private(path)?
        .trim()
        .to_string();
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_graphic()) {
        bail!("management key must be nonempty ASCII text without whitespace");
    }
    Ok(key)
}

fn ensure_key(path: &Path) -> Result<String> {
    if path.try_exists()? {
        return read_key(path);
    }
    let mut random = [0_u8; 32];
    getrandom::getrandom(&mut random)
        .map_err(|error| anyhow::anyhow!("generate management key: {error}"))?;
    let key: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    usagestat_core::storage::create_once(path, format!("{key}\n").as_bytes())?;
    read_key(path)
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    usagestat_core::storage::write_atomic(path, contents.as_bytes())?;
    Ok(())
}

fn local_client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Ok(reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()?)
}

fn quota_endpoint_available(url: &str, key_file: &Path) -> bool {
    let check = || -> Result<bool> {
        let key = read_key(key_file)?;
        let response = local_client(Duration::from_secs(1))?
            .get(format!("{url}/v0/management/quota-scheduler/status"))
            .bearer_auth(key)
            .send()?;
        if !response.status().is_success() {
            return Ok(false);
        }
        let body: serde_json::Value = response.json()?;
        Ok(body
            .get("accounts")
            .is_some_and(|accounts| accounts.is_object()))
    };
    check().unwrap_or(false)
}

fn capture_provider_roots(
    saved: &mut BTreeMap<String, String>,
    read: impl Fn(&str) -> Option<String>,
) -> Result<()> {
    for name in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "CURSOR_STATE_DB", "CURSOR_NIGHTLY_STATE_DB", "CURSOR_AGENT_HOME", "FLATPAK_XDG_CONFIG_HOME"] {
        if let Some(value) = read(name) {
            if value.is_empty() {
                saved.remove(name);
            } else {
                // Relative roots change meaning at login. Expanding them also
                // changes Claude's raw-path credential hash, so require the
                // user/upstream application to agree on an absolute spelling.
                anyhow::ensure!(Path::new(&value).is_absolute(), "{name} must be an absolute provider path before enabling startup at login");
                saved.insert(name.to_owned(), value);
            }
        }
    }
    // OpenCode interprets relative database names beneath its XDG data root,
    // not the process working directory. Preserve that upstream selection.
    if let Some(value) = read("OPENCODE_DB") {
        anyhow::ensure!(!value.contains('\0'), "OPENCODE_DB contains an invalid path character");
        if value.is_empty() { saved.remove("OPENCODE_DB"); }
        else { saved.insert("OPENCODE_DB".into(), value); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::systemd::*;
    use super::*;
    use std::io::Write;
    fn temp_dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "usagestat-service-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn login_environment_preserves_only_explicit_absolute_provider_roots() {
        let root = temp_dir().join("profile 使用 with spaces").to_string_lossy().into_owned();
        let mut saved = BTreeMap::from([("CODEX_HOME".into(), root.clone())]);
        capture_provider_roots(&mut saved, |name| match name {
            "CLAUDE_CONFIG_DIR" => Some(root.clone()),
            "CODEX_HOME" => Some(String::new()),
            "API_KEY" => panic!("secret environment requested"),
            _ => None,
        }).unwrap();
        assert_eq!(saved, BTreeMap::from([("CLAUDE_CONFIG_DIR".into(), root.clone())]));
        capture_provider_roots(&mut saved, |_| None).unwrap();
        assert!(capture_provider_roots(&mut saved, |name| (name == "CODEX_HOME").then(|| "relative-profile".into())).is_err());
        assert!(!saved.contains_key("CODEX_HOME"));
        capture_provider_roots(&mut saved, |name| match name {
            "OPENCODE_DB" => Some("channel.db".into()),
            "FLATPAK_XDG_CONFIG_HOME" => Some(root.clone()),
            "OPENCODE_AUTH_CONTENT" => panic!("inline provider credentials requested"),
            _ => None,
        }).unwrap();
        assert_eq!(saved["OPENCODE_DB"], "channel.db");
        assert_eq!(saved["FLATPAK_XDG_CONFIG_HOME"], root);
        fs::remove_dir_all(Path::new(&root).parent().unwrap()).unwrap();
    }

    #[test]
    fn generates_private_persistent_key_without_rotation() {
        let dir = temp_dir();
        let path = dir.join("key");
        let key = ensure_key(&path).unwrap();
        assert_eq!(key.len(), 64);
        assert!(key.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(ensure_key(&path).unwrap(), key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&path, "invalid key\n").unwrap();
        assert!(ensure_key(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "invalid key\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn service_arguments_escape_systemd_expansion_and_reject_injection() {
        assert_eq!(
            unit_quote("/path with space/$VAR/%h/\"file\"", true).unwrap(),
            "\"/path with space/$$VAR/%%h/\\\"file\\\"\""
        );
        assert_eq!(
            unit_quote("PATH=/a/$literal/%path", false).unwrap(),
            "\"PATH=/a/$literal/%%path\""
        );
        assert!(unit_quote("/path\nExecStart=/evil", true).is_err());
        let unit = render_unit(
            Path::new("/opt/my tools/usagestatd"),
            Path::new("/config.toml"),
            &[PathBuf::from("/my plugins")],
            Some(Path::new("/key")),
            "127.0.0.1:6736".parse().unwrap(),
        )
        .unwrap();
        assert!(unit.contains("ExecStart=\"/opt/my tools/usagestatd\" --bind 127.0.0.1:6736"));
        assert!(unit.contains("--plugin-dir \"/my plugins\""));
        assert!(unit.contains("--management-key-file \"/key\""));
        assert!(unit_enables_t3(&unit, Path::new("/key")).unwrap());
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn ordinary_service_keeps_t3_disabled_even_with_a_retained_key() {
        let dir = temp_dir();
        let key = dir.join("key");
        fs::write(&key, "invalid old key\n").unwrap();
        let unit = render_unit(
            Path::new("/usr/bin/usagestatd"),
            Path::new("/config --management-key-file \"/key\".toml"),
            &[],
            None,
            "127.0.0.1:6736".parse().unwrap(),
        )
        .unwrap();
        assert!(!unit_enables_t3(&unit, &key).unwrap());
        assert!(!unit_enables_t3(&unit, Path::new("/key")).unwrap());
        assert!(unit.contains("\nUnsetEnvironment=USAGESTAT_MANAGEMENT_KEY\n"));
        assert_eq!(fs::read_to_string(&key).unwrap(), "invalid old key\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_explicit_bridge_controls_and_plain_daemon_commands() {
        use clap::Parser;
        for args in [
            vec!["usagestat", "daemon", "t3", "auto"],
            vec!["usagestat", "daemon", "t3", "off"],
            vec!["usagestat", "daemon", "toggle", "--t3"],
            vec!["usagestat", "daemon", "disable", "--t3"],
            vec!["usagestat", "daemon", "disable"],
            vec!["usagestat", "daemon", "enable"],
            vec!["usagestat", "daemon", "start"],
            vec!["usagestat", "daemon", "stop"],
            vec!["usagestat", "daemon", "autostart", "on"],
            vec!["usagestat", "daemon", "autostart", "off"],
            vec!["usagestat", "daemon", "unregister"],
        ] {
            assert!(crate::Cli::try_parse_from(args).is_ok());
        }
        assert!(crate::Cli::try_parse_from(["usagestat", "daemon", "toggle"]).is_err());
        assert!(crate::Cli::try_parse_from(["usagestat", "daemon", "t3", "on"]).is_err());
        assert!(crate::Cli::try_parse_from(["usagestat", "daemon", "t3"]).is_err());
    }

    #[test]
    fn status_distinguishes_saved_mode_from_connection_availability() {
        assert_eq!(
            T3Mode::Auto.status_text(false, false),
            "auto · unavailable (daemon stopped)"
        );
        assert_eq!(T3Mode::Auto.status_text(true, true), "auto · available");
        assert_eq!(
            T3Mode::Auto.status_text(true, false),
            "auto · unavailable (bridge not responding)"
        );
        for running in [false, true] {
            assert_eq!(T3Mode::Off.status_text(running, false), "off");
        }
    }

    #[test]
    fn availability_requires_an_authenticated_quota_response() {
        let dir = temp_dir();
        let key = dir.join("key");
        fs::write(&key, "test-key").unwrap();
        for (status, body, expected) in [
            ("200 OK", r#"{"accounts":{}}"#, true),
            ("401 Unauthorized", r#"{"accounts":{}}"#, false),
            ("200 OK", r#"{"status":"ok"}"#, false),
            ("200 OK", "invalid json", false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    assert!(std::io::BufRead::read_line(&mut reader, &mut line).unwrap() > 0);
                    request.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                assert!(request.starts_with("GET /v0/management/quota-scheduler/status "));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer test-key\r\n")
                );
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            assert_eq!(quota_endpoint_available(&url, &key), expected);
            server.join().unwrap();
        }
        assert!(!quota_endpoint_available(
            "http://127.0.0.1:1",
            &dir.join("missing")
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn preserves_unmanaged_services() {
        let dir = temp_dir();
        let path = dir.join(UNIT);
        assert!(ensure_managed_unit(&path).is_ok());
        fs::write(&path, "[Service]\nExecStart=/unrelated\n").unwrap();
        assert!(ensure_managed_unit(&path).is_err());
        write_atomic(&path, &format!("{MARKER}[Service]\n")).unwrap();
        assert!(ensure_managed_unit(&path).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resolves_local_urls_for_wildcards_and_ipv6() {
        for (bind, expected) in [
            ("0.0.0.0:6736", "http://127.0.0.1:6736"),
            ("[::]:6736", "http://[::1]:6736"),
            ("[::1]:7000", "http://[::1]:7000"),
        ] {
            assert_eq!(local_url(bind.parse().unwrap()), expected);
        }
    }
}
