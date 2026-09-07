//! Linux user service adapter and legacy unit migration.
use super::*;
use std::process::Output;

pub(super) const UNIT: &str = "usagestat.service";
pub(super) const MARKER: &str = "# Managed by usagestat daemon enable\n";

pub(super) fn service_name() -> &'static str {
    if dev_profile() {
        "usagestat-dev.service"
    } else {
        UNIT
    }
}

pub(super) fn service_unit_file() -> Result<PathBuf> {
    // A GUI app can override XDG_CONFIG_HOME without changing the login
    // manager's search path. Discover that path through its structured API.
    if let Ok(paths) = manager_unit_paths() {
        if let Some(directory) = paths.into_iter().find(|p| p.ends_with("systemd/user")) {
            return Ok(directory.join(service_name()));
        }
    }
    Ok(dirs::config_dir()
        .context("locate user config directory")?
        .join("systemd/user")
        .join(service_name()))
}

fn manager_unit_paths() -> Result<Vec<PathBuf>> {
    let mut command = usagestat_core::process::command("busctl")?;
    command.args([
        "--user",
        "--json=short",
        "get-property",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "UnitPath",
    ]);
    let output = usagestat_core::process::run(command, Duration::from_secs(2), 32 * 1024)?;
    if !output.status.success() {
        bail!("the systemd user manager is unavailable");
    }
    #[derive(Deserialize)]
    struct Property {
        data: Vec<PathBuf>,
    }
    Ok(serde_json::from_slice::<Property>(&output.stdout)?.data)
}

pub(super) fn systemctl(args: &[&str]) -> Result<Output> {
    let mut command = usagestat_core::process::command("systemctl")?;
    command.arg("--user").args(args);
    let output = usagestat_core::process::run(command, Duration::from_secs(20), 64 * 1024)
        .context("run systemctl --user; a systemd user session is required")?;
    if !output.status.success() {
        bail!(
            "systemctl --user {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

pub(super) fn read_optional_unit(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(unit) => Ok(unit),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).context("read daemon service configuration"),
    }
}

#[cfg(test)]
pub(super) fn unit_enables_t3(unit: &str, key: &Path) -> Result<bool> {
    let argument = format!(" --management-key-file {}", path_arg(key)?);
    Ok(unit.starts_with(MARKER)
        && unit
            .lines()
            .any(|line| line.starts_with("ExecStart=") && line.contains(&argument)))
}

pub(super) fn unit_words(command: &str) -> Result<Vec<&str>> {
    let mut words = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in command.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if let Some(delimiter) = quote {
            if c == delimiter {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c.is_whitespace() {
            if let Some(begin) = start.take() {
                words.push(&command[begin..i]);
            }
            continue;
        }
        start.get_or_insert(i);
    }
    if quote.is_some() || escaped {
        bail!("invalid quoting in managed daemon service");
    }
    if let Some(begin) = start {
        words.push(&command[begin..]);
    }
    Ok(words)
}

pub(super) fn unit_command(unit: &str) -> Result<&str> {
    let commands: Vec<_> = unit
        .lines()
        .filter_map(|line| line.strip_prefix("ExecStart="))
        .collect();
    let [command] = commands.as_slice() else {
        bail!("expected one ExecStart in managed daemon service");
    };
    Ok(command)
}

pub(super) fn ensure_managed_unit(path: &Path) -> Result<()> {
    match fs::read_to_string(path) {
        Ok(text) if !text.starts_with(MARKER) => bail!(
            "{} is not managed by usagestat; preserve or move that service before using this command",
            path.display()
        ),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => Ok(()),
    }
}

pub(super) fn unit_quote(value: &str, exec: bool) -> Result<String> {
    if value.chars().any(|c| c.is_control()) {
        bail!("service paths and environment values must not contain control characters");
    }
    let value = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    Ok(format!(
        "\"{}\"",
        if exec {
            value.replace('$', "$$")
        } else {
            value
        }
    ))
}

pub(super) fn path_arg(path: &Path) -> Result<String> {
    unit_quote(
        path.to_str().context("service paths must be valid UTF-8")?,
        true,
    )
}

pub(super) fn render_unit(
    binary: &Path,
    config: &Path,
    plugin_dirs: &[PathBuf],
    key: Option<&Path>,
    bind: SocketAddr,
) -> Result<String> {
    let mut command = format!(
        "{} --bind {} --config {}",
        path_arg(binary)?,
        bind,
        path_arg(config)?
    );
    if let Some(key) = key {
        command.push_str(&format!(" --management-key-file {}", path_arg(key)?));
    }
    for dir in plugin_dirs {
        command.push_str(&format!(" --plugin-dir {}", path_arg(dir)?));
    }
    let mut environment = String::new();
    // Preserve CLI discovery and XDG locations; never persist the invoking
    // process's full environment (it can contain provider credentials).
    for name in ["PATH", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        if let Ok(value) = std::env::var(name) {
            environment.push_str(&format!(
                "Environment={}\n",
                unit_quote(&format!("{name}={value}"), false)?
            ));
        }
    }
    let env_file = paths::config_dir()?.join("daemon.env");
    let env_file = unit_quote(
        env_file
            .to_str()
            .context("environment file path must be UTF-8")?,
        false,
    )?;
    Ok(format!(
        "{MARKER}[Unit]\nDescription=usagestat usage backend\n\n[Service]\nType=exec\nExecStart={command}\nWorkingDirectory=%h\n{environment}EnvironmentFile=-{env_file}\nUnsetEnvironment=USAGESTAT_MANAGEMENT_KEY\nRestart=on-failure\nRestartSec=5\nUMask=0077\n\n[Install]\nWantedBy=default.target\n"
    ))
}

pub(super) struct Systemd {
    file: PathBuf,
    name: String,
}
impl Systemd {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            file: service_unit_file()?,
            name: service_name().to_owned(),
        })
    }
}

impl ServiceManager for Systemd {
    fn kind(&self) -> &'static str {
        "systemd"
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn file(&self) -> Option<PathBuf> {
        Some(self.file.clone())
    }
    fn validate(&self) -> Result<()> {
        ensure_managed_unit(&self.file)?;
        systemctl(&["show-environment"])?;
        if !self.file.exists() {
            let output = systemctl(&[
                "show",
                self.name.as_str(),
                "--property=LoadState",
                "--value",
            ])?;
            let loaded = String::from_utf8_lossy(&output.stdout);
            if loaded.trim() != "not-found" {
                bail!(
                    "a service registered outside {} already owns {}; preserve that service",
                    self.file.display(),
                    self.name.as_str()
                );
            }
        }
        Ok(())
    }
    fn migrate(&self) -> Result<Option<DaemonSettings>> {
        legacy_settings(&read_optional_unit(&self.file)?)
    }
    fn query(&self) -> Result<Registration> {
        let output = systemctl(&[
            "show",
            self.name.as_str(),
            "--property=LoadState,ActiveState,UnitFileState",
        ])?;
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(Registration {
            registered: text
                .lines()
                .any(|s| s.starts_with("LoadState=") && s != "LoadState=not-found"),
            enabled: text.lines().any(|s| s == "UnitFileState=enabled"),
            running: text
                .lines()
                .any(|s| s == "ActiveState=active" || s == "ActiveState=activating"),
        })
    }
    fn install(&self, installation: &Installation, settings: &Path) -> Result<()> {
        ensure_managed_unit(&self.file)?;
        let old = read_optional_unit(&self.file)?;
        let base = if old.is_empty() {
            render_unit(
                &installation.binary,
                &installation.config,
                &installation.plugin_dirs,
                None,
                installation.bind,
            )?
        } else {
            old
        };
        let unit = managed_unit(&base, &installation.binary, settings)?;
        write_atomic(&self.file, &unit)?;
        systemctl(&["daemon-reload"])?;
        Ok(())
    }
    fn enable(&self) -> Result<()> {
        systemctl(&["enable", self.name.as_str()])?;
        systemctl(&["restart", self.name.as_str()])?;
        Ok(())
    }
    fn disable(&self) -> Result<()> {
        if self.query()?.registered {
            systemctl(&["disable", "--now", self.name.as_str()])?;
        }
        Ok(())
    }
    fn restart(&self) -> Result<()> {
        systemctl(&["try-restart", self.name.as_str()])?;
        Ok(())
    }
    fn unregister(&self) -> Result<()> {
        self.validate()?;
        if self.file.exists() {
            self.disable()?;
            ensure_managed_unit(&self.file)?;
            fs::remove_file(&self.file).context("remove managed user service file")?;
            systemctl(&["daemon-reload"])?;
        }
        Ok(())
    }
    fn start(&self) -> Result<()> {
        self.validate()?;
        systemctl(&["start", self.name.as_str()])?;
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.validate()?;
        systemctl(&["stop", self.name.as_str()])?;
        Ok(())
    }
    fn set_autostart(&self, enabled: bool) -> Result<()> {
        self.validate()?;
        systemctl(&[if enabled { "enable" } else { "disable" }, self.name.as_str()])?;
        Ok(())
    }
}

fn managed_unit(unit: &str, binary: &Path, settings: &Path) -> Result<String> {
    unit_command(unit)?; // Reject ambiguous/multiple commands before changing it.
    let command = format!(
        "ExecStart={} --service-settings {}",
        path_arg(binary)?,
        path_arg(settings)?
    );
    let mut updated = String::new();
    for line in unit.lines() {
        updated.push_str(if line.starts_with("ExecStart=") {
            &command
        } else {
            line
        });
        updated.push('\n');
        if line == "[Service]" && !unit.contains("\nUnsetEnvironment=USAGESTAT_MANAGEMENT_KEY\n") {
            updated.push_str("UnsetEnvironment=USAGESTAT_MANAGEMENT_KEY\n");
        }
    }
    Ok(updated)
}

fn decoded_word(encoded: &str, exec: bool) -> Result<String> {
    let mut value = String::new();
    let mut quote = None;
    let mut chars = encoded.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next().context("incomplete service escape")? {
                c @ ('\\' | '"' | '\'') => value.push(c),
                _ => bail!(
                    "legacy service uses an unsupported escape; keep its unit and migrate that value explicitly"
                ),
            },
            '\'' | '"' if quote.is_none() => quote = Some(c),
            c if quote == Some(c) => quote = None,
            '%' => {
                if chars.next() != Some('%') {
                    bail!(
                        "legacy service uses dynamic percent expansion; resolve that value before migration"
                    );
                }
                value.push('%');
            }
            '$' if exec => {
                if chars.next() != Some('$') {
                    bail!(
                        "legacy service uses dynamic environment expansion; resolve that value before migration"
                    );
                }
                value.push('$');
            }
            c => value.push(c),
        }
    }
    if quote.is_some() {
        bail!("unterminated service quote");
    }
    Ok(value)
}

fn legacy_settings(unit: &str) -> Result<Option<DaemonSettings>> {
    if !unit.starts_with(MARKER) {
        return Ok(None);
    }
    let words = unit_words(unit_command(unit)?)?
        .iter()
        .map(|word| decoded_word(word, true))
        .collect::<Result<Vec<_>>>()?;
    if words.iter().any(|word| word == "--service-settings") {
        bail!(
            "managed daemon settings are missing; restore daemon.json before changing its service"
        );
    }
    let binary = PathBuf::from(words.first().context("missing legacy daemon executable")?);
    let mut bind = None;
    let mut config = None;
    let mut plugin_dirs = Vec::new();
    let mut management_key = None;
    let mut i = 1;
    while i < words.len() {
        let value = words
            .get(i + 1)
            .context("missing legacy daemon argument value")?;
        match words[i].as_str() {
            "--bind" => bind = Some(value.parse::<SocketAddr>().context("invalid legacy bind")?),
            "--config" => config = Some(PathBuf::from(value)),
            "--plugin-dir" => plugin_dirs.push(PathBuf::from(value)),
            "--management-key-file" => management_key = Some(PathBuf::from(value)),
            _ => bail!(
                "legacy daemon has an additional argument {}; preserve the unit and migrate it explicitly",
                words[i]
            ),
        }
        i += 2;
    }
    let mut environment = BTreeMap::new();
    for line in unit
        .lines()
        .filter_map(|line| line.strip_prefix("Environment="))
    {
        for word in unit_words(line)? {
            let decoded = decoded_word(word, false)?;
            let (key, value) = decoded
                .split_once('=')
                .context("invalid legacy environment entry")?;
            if key != "USAGESTAT_MANAGEMENT_KEY" {
                environment.insert(key.to_owned(), value.to_owned());
            }
        }
    }
    for (name, path) in [
        ("USAGESTAT_CONFIG_DIR", paths::config_dir()?),
        ("USAGESTAT_DATA_DIR", paths::data_dir()?),
    ] {
        environment.entry(name.to_owned()).or_insert(
            absolute(&path)?
                .to_str()
                .context("service paths must be UTF-8")?
                .to_owned(),
        );
    }
    Ok(Some(DaemonSettings {
        t3_mode: if management_key.is_some() {
            SavedT3Mode::Auto
        } else {
            SavedT3Mode::Off
        },
        installation: Some(Installation {
            owner: installation_owner(&binary)?,
            binary,
            bind: bind.context("legacy service has no bind address")?,
            config: config.context("legacy service has no configuration path")?,
            plugin_dirs,
            environment,
            management_key_file: management_key.unwrap_or(paths::management_key_file()?),
            control_key_file: paths::control_key_file()?,
        }),
    }))
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    #[test]
    fn migrates_legacy_paths_and_preserves_custom_unit_options() {
        let key = Path::new("/state/工具 %h $literal key");
        let old = render_unit(
            Path::new("/opt/tools/usagestatd"),
            Path::new("/config/quoted \"file\".toml"),
            &[PathBuf::from("/plugins/使用 space/$literal/%h")],
            Some(key),
            "[::1]:7345".parse().unwrap(),
        )
        .unwrap()
        .replace(
            "RestartSec=5",
            "RestartSec=7\nEnvironment=\"EXPLICIT_VALUE=retained\"\nNice=5",
        );
        let migrated = legacy_settings(&old).unwrap().unwrap();
        assert_eq!(migrated.t3_mode, SavedT3Mode::Auto);
        let install = migrated.installation.as_ref().unwrap();
        assert_eq!(install.bind.to_string(), "[::1]:7345");
        assert_eq!(install.config, Path::new("/config/quoted \"file\".toml"));
        assert_eq!(
            install.plugin_dirs,
            [PathBuf::from("/plugins/使用 space/$literal/%h")]
        );
        assert_eq!(install.management_key_file, key);
        assert_eq!(install.environment["EXPLICIT_VALUE"], "retained");
        let updated = managed_unit(
            &old,
            &install.binary,
            Path::new("/new settings/daemon.json"),
        )
        .unwrap();
        for line in old.lines().filter(|line| !line.starts_with("ExecStart=")) {
            assert!(updated.lines().any(|other| other == line));
        }
        assert!(updated.contains("--service-settings \"/new settings/daemon.json\""));
        let directory = usagestat_core::storage::temporary_directory().unwrap();
        let path = directory.path().join("daemon.json");
        migrated.save(&path).unwrap();
        assert_eq!(DaemonSettings::load(&path).unwrap(), Some(migrated));
    }

    #[test]
    fn migration_does_not_guess_at_unmanaged_or_dynamic_commands() {
        assert!(
            legacy_settings("[Service]\nExecStart=/other\n")
                .unwrap()
                .is_none()
        );
        for command in [
            "/daemon --bind $ADDRESS --config /config",
            "/daemon --bind 127.0.0.1:1 --config %h/config",
            "/daemon --service-settings /missing",
        ] {
            assert!(legacy_settings(&format!("{MARKER}[Service]\nExecStart={command}\n")).is_err());
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod live_tests {
    use super::*;

    struct Installed<'a>(&'a Systemd);
    impl Drop for Installed<'_> {
        fn drop(&mut self) {
            let _ = self.0.disable();
            let _ = fs::remove_file(&self.0.file);
            let _ = systemctl(&["daemon-reload"]);
            let _ = systemctl(&["reset-failed", &self.0.name]);
        }
    }

    #[test]
    #[ignore = "requires an available systemd user session and USAGESTAT_TEST_DAEMON_BINARY"]
    fn isolated_real_user_service_lifecycle() {
        let binary = PathBuf::from(
            std::env::var_os("USAGESTAT_TEST_DAEMON_BINARY").expect("set the built daemon path"),
        );
        assert!(binary.is_file());
        let directory = usagestat_core::storage::temporary_directory().unwrap();
        let root = directory.path();
        let name = format!("usagestat-native-test-{}.service", std::process::id());
        let manager = Systemd {
            file: service_unit_file().unwrap().with_file_name(&name),
            name,
        };
        assert!(
            !manager.file.exists(),
            "test service name is already in use"
        );
        manager.validate().unwrap();
        let _cleanup = Installed(&manager);
        let config = root.join("config.toml");
        fs::write(&config, "providers = []\n").unwrap();
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let bind = held.local_addr().unwrap();
        drop(held);
        let key = root.join("management-key");
        let control = root.join("control-key");
        ensure_key(&control).unwrap();
        let mut settings = DaemonSettings {
            t3_mode: SavedT3Mode::Off,
            installation: Some(Installation {
                owner: root.to_owned(),
                binary,
                bind,
                config,
                plugin_dirs: vec![],
                environment: BTreeMap::from([
                    (
                        "USAGESTAT_CONFIG_DIR".into(),
                        root.join("config").to_str().unwrap().into(),
                    ),
                    (
                        "USAGESTAT_DATA_DIR".into(),
                        root.join("data").to_str().unwrap().into(),
                    ),
                    ("HOME".into(), root.to_str().unwrap().into()),
                ]),
                management_key_file: key.clone(),
                control_key_file: control,
            }),
        };
        let saved = root.join("daemon.json");
        settings.save(&saved).unwrap();
        manager
            .install(settings.installation.as_ref().unwrap(), &saved)
            .unwrap();
        // Keep fixture discovery outside the real user's home, including CWD fallback.
        let unit = read_optional_unit(&manager.file).unwrap().replace(
            "WorkingDirectory=%h",
            &format!(
                "WorkingDirectory={}",
                root.to_str().unwrap().replace('%', "%%")
            ),
        );
        write_atomic(&manager.file, &unit).unwrap();
        systemctl(&["daemon-reload"]).unwrap();
        manager.enable().unwrap();
        wait_for_installation(settings.installation.as_ref().unwrap()).unwrap();
        let state = manager.query().unwrap();
        assert!(state.registered && state.running && state.enabled);
        super::lifecycle_tests::independent_controls(&manager, settings.installation.as_ref().unwrap());
        manager.disable().unwrap();
        assert!(!manager.query().unwrap().running);
        assert!(!apply_t3(&mut settings, SavedT3Mode::Auto, &saved, &key, &manager).unwrap());
        assert!(!manager.query().unwrap().running);
        let retained = read_key(&key).unwrap();
        manager.enable().unwrap();
        wait_for_installation(settings.installation.as_ref().unwrap()).unwrap();
        assert!(quota_endpoint_available(&local_url(bind), &key));
        assert!(apply_t3(&mut settings, SavedT3Mode::Off, &saved, &key, &manager).unwrap());
        wait_for_installation(settings.installation.as_ref().unwrap()).unwrap();
        assert!(!quota_endpoint_available(&local_url(bind), &key));
        assert!(read_key(&key).unwrap() == retained);
        manager.disable().unwrap();
        let state = manager.query().unwrap();
        assert!(!state.running && !state.enabled);
        assert!(saved.exists() && key.exists());
        manager.unregister().unwrap();
        manager.unregister().unwrap();
        let removed = manager.query().unwrap();
        assert!(!removed.registered && !removed.running && !removed.enabled);
        assert!(saved.exists() && key.exists());
    }
}
