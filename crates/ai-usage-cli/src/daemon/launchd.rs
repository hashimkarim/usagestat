//! Per-user macOS LaunchAgent adapter. No system LaunchDaemon or elevation.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
use super::*;
use plist::{Dictionary, Value};
use std::process::Output;

pub(super) struct LaunchAgent {
    label: String,
    file: PathBuf,
    domain: String,
    settings: PathBuf,
}

impl LaunchAgent {
    pub(super) fn new() -> Result<Self> {
        let mut command = usagestat_core::process::command("/usr/bin/id")?;
        command.arg("-u");
        let user = usagestat_core::process::run(command, Duration::from_secs(2), 128)?;
        let uid: u32 = String::from_utf8(user.stdout)?
            .trim()
            .parse()
            .context("get current macOS user ID")?;
        let label = if dev_profile() {
            "com.usagestat.daemon.dev"
        } else {
            "com.usagestat.daemon"
        }
        .to_owned();
        Ok(Self {
            file: paths::home_dir()
                .context("locate the macOS user home")?
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist")),
            label,
            domain: format!("gui/{uid}"),
            settings: settings_file()?,
        })
    }
    fn target(&self) -> String {
        format!("{}/{}", self.domain, self.label)
    }
    fn read(&self) -> Result<Option<Dictionary>> {
        let text = match fs::read(&self.file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("read LaunchAgent"),
        };
        let value =
            Value::from_reader(std::io::Cursor::new(text)).context("read LaunchAgent plist")?;
        let dictionary = value
            .into_dictionary()
            .context("LaunchAgent is not a dictionary")?;
        if !owns_plist(&dictionary, &self.label) {
            bail!(
                "{} is not managed by usagestat; preserve that agent",
                self.file.display()
            );
        }
        Ok(Some(dictionary))
    }
    fn write(&self, dictionary: Dictionary) -> Result<()> {
        let mut bytes = Vec::new();
        Value::Dictionary(dictionary).to_writer_xml(&mut bytes)?;
        usagestat_core::storage::write_atomic(&self.file, &bytes)?;
        Ok(())
    }
    fn domain_available(&self) -> Result<()> {
        launchctl(&["print", &self.domain]).context(
            "a logged-in macOS GUI session is required; foreground usagestatd remains available",
        )?;
        Ok(())
    }
    fn stop_loaded(&self) -> Result<()> {
        if job_dictionary(&self.label)?.is_some() {
            // bootout sends the registered job SIGTERM. ExitTimeout bounds it,
            // and the backend's signal handler cancels its helper trees.
            let target = self.target();
            let output = launchctl_output(&["bootout", &target])?;
            // Removal is asynchronous, and another bootout can report ESRCH
            // while the native job dictionary still exposes the exiting job.
            if !output.status.success() && output.status.code() != Some(3) {
                bail!(
                    "launchctl bootout {target} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            self.domain_available()?;
            let deadline = Instant::now() + Duration::from_secs(8);
            while job_dictionary(&self.label)?.is_some() {
                if Instant::now() >= deadline {
                    bail!("LaunchAgent removal did not finish for {target}");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    }
    fn set_disabled(&self, disabled: bool) -> Result<()> {
        if let Some(mut plist) = self.read()? {
            plist.insert("Disabled".into(), Value::Boolean(disabled));
            self.write(plist)?;
        }
        Ok(())
    }
}

impl ServiceManager for LaunchAgent {
    fn kind(&self) -> &'static str {
        "launchd"
    }
    fn name(&self) -> String {
        self.label.clone()
    }
    fn file(&self) -> Option<PathBuf> {
        Some(self.file.clone())
    }
    fn validate(&self) -> Result<()> {
        self.domain_available()?;
        let file = self.read()?;
        if let Some(job) = job_dictionary(&self.label)? {
            let Some(file) = file else {
                bail!(
                    "{} is already registered by another LaunchAgent; preserve it",
                    self.label
                );
            };
            if job.get("ProgramArguments") != file.get("ProgramArguments") {
                bail!(
                    "the loaded LaunchAgent does not match {}; preserve it before changing installation ownership",
                    self.file.display()
                );
            }
        }
        Ok(())
    }
    fn query(&self) -> Result<Registration> {
        self.domain_available()?;
        let file = self.read()?;
        let job = job_dictionary(&self.label)?;
        let mut disabled = file
            .as_ref()
            .and_then(|d| d.get("Disabled"))
            .and_then(Value::as_boolean)
            .unwrap_or(true);
        let overrides = launchctl(&["print-disabled", &self.domain])?;
        if let Some(value) =
            disabled_override(&String::from_utf8_lossy(&overrides.stdout), &self.label)?
        {
            disabled = value;
        }
        Ok(Registration {
            registered: file.is_some() || job.is_some(),
            enabled: file.is_some() && !disabled,
            running: job
                .as_ref()
                .and_then(|d| d.get("PID"))
                .and_then(Value::as_unsigned_integer)
                .is_some_and(|pid| pid > 0),
        })
    }
    fn install(&self, installation: &Installation, settings: &Path) -> Result<()> {
        if settings != self.settings {
            bail!("LaunchAgent settings path does not match this profile");
        }
        let old = self.read()?;
        let disabled = old
            .as_ref()
            .and_then(|d| d.get("Disabled"))
            .and_then(Value::as_boolean)
            .unwrap_or(true);
        let log_root = PathBuf::from(
            installation
                .environment
                .get("USAGESTAT_DATA_DIR")
                .context("saved local data directory is missing")?,
        )
        .join("logs");
        usagestat_core::storage::private_directory(&log_root)?;
        for name in ["daemon.stdout.log", "daemon.stderr.log"] {
            usagestat_core::storage::append_private(&log_root.join(name), b"")?;
        }
        self.write(render_plist(
            &self.label,
            installation,
            settings,
            &log_root,
            disabled,
        )?)
    }
    fn enable(&self) -> Result<()> {
        self.stop_loaded()?;
        self.set_disabled(false)?;
        launchctl(&["enable", &self.target()])?;
        launchctl(&[
            "bootstrap",
            &self.domain,
            self.file
                .to_str()
                .context("LaunchAgent path must be UTF-8")?,
        ])?;
        Ok(())
    }
    fn disable(&self) -> Result<()> {
        if self.read()?.is_some() {
            launchctl(&["disable", &self.target()])?;
            self.stop_loaded()?;
            self.set_disabled(true)?;
        }
        Ok(())
    }
    fn unregister(&self) -> Result<()> {
        self.validate()?;
        self.disable()?;
        if self.read()?.is_some() {
            fs::remove_file(&self.file).context("remove managed LaunchAgent plist")?;
        }
        Ok(())
    }
    fn set_autostart(&self, enabled: bool) -> Result<()> {
        self.validate()?;
        anyhow::ensure!(self.read()?.is_some(), "LaunchAgent is not registered");
        let previous = self.query()?.enabled;
        self.set_disabled(!enabled)?;
        if let Err(error) = launchctl(&[if enabled { "enable" } else { "disable" }, &self.target()]) {
            // Keep the on-disk and launchd preferences coherent if the native
            // update fails. Report a failed rollback rather than hiding it.
            self.set_disabled(!previous).context("restore the LaunchAgent startup preference")?;
            launchctl(&[if previous { "enable" } else { "disable" }, &self.target()])
                .context("restore the native LaunchAgent startup preference")?;
            return Err(error);
        }
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.validate()?;
        self.stop_loaded()
    }
    fn start(&self) -> Result<()> {
        self.validate()?;
        let previous = self.query()?;
        if previous.running { return Ok(()); }
        anyhow::ensure!(previous.registered, "LaunchAgent is not registered");
        self.stop_loaded()?;
        self.set_autostart(true)?;
        let started = (|| -> Result<()> {
            launchctl(&["bootstrap", &self.domain, self.file.to_str().context("LaunchAgent path must be UTF-8")?])?;
            // Ensure RunAtLoad has started before restoring a disabled login
            // preference. Disabling a loaded running job does not stop it.
            let deadline = Instant::now() + Duration::from_secs(8);
            while !self.query()?.running {
                anyhow::ensure!(Instant::now() < deadline, "LaunchAgent did not start within its bounded deadline");
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        })();
        let stopped = if started.is_err() { self.stop_loaded() } else { Ok(()) };
        let restored = self.set_autostart(previous.enabled);
        stopped.context("restore the stopped LaunchAgent after failed startup")?;
        restored.context("restore the LaunchAgent login preference after startup")?;
        started
    }
    fn restart(&self) -> Result<()> {
        // Reload the plist as well as the saved settings, so upgraded absolute
        // binary paths take effect. Preserve disabled/autostart intent.
        self.stop_loaded()?;
        // Starting a disabled job requires temporary enablement. Reuse the
        // same restoration path as an explicit start so toggling T3 while
        // running with login startup disabled keeps that state intact.
        self.start()
    }
}

fn launchctl(args: &[&str]) -> Result<Output> {
    let output = launchctl_output(args)?;
    if !output.status.success() {
        bail!(
            "launchctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

fn launchctl_output(args: &[&str]) -> Result<Output> {
    let mut command = usagestat_core::process::command("/bin/launchctl")?;
    command.args(args);
    let output = usagestat_core::process::run(command, Duration::from_secs(15), 256 * 1024)?;
    Ok(output)
}

fn string(path: &Path) -> Result<Value> {
    let value = path.to_str().context("LaunchAgent paths must be UTF-8")?;
    if !path.is_absolute() || value.chars().any(char::is_control) {
        bail!("LaunchAgent paths must be absolute without control characters");
    }
    Ok(Value::String(value.to_owned()))
}

fn render_plist(
    label: &str,
    installation: &Installation,
    settings: &Path,
    logs: &Path,
    disabled: bool,
) -> Result<Dictionary> {
    let mut values = Dictionary::new();
    values.insert("Label".into(), Value::String(label.to_owned()));
    values.insert(
        "ProgramArguments".into(),
        Value::Array(vec![
            string(&installation.binary)?,
            Value::String("--service-settings".into()),
            string(settings)?,
        ]),
    );
    values.insert(
        "WorkingDirectory".into(),
        string(settings.parent().context("settings need a parent")?)?,
    );
    values.insert(
        "StandardOutPath".into(),
        string(&logs.join("daemon.stdout.log"))?,
    );
    values.insert(
        "StandardErrorPath".into(),
        string(&logs.join("daemon.stderr.log"))?,
    );
    values.insert(
        "EnvironmentVariables".into(),
        Value::Dictionary(Dictionary::from_iter([(
            "USAGESTAT_MANAGED_SERVICE",
            Value::String("1".into()),
        )])),
    );
    values.insert("RunAtLoad".into(), Value::Boolean(true));
    values.insert("Disabled".into(), Value::Boolean(disabled));
    values.insert(
        "KeepAlive".into(),
        Value::Dictionary(Dictionary::from_iter([(
            "SuccessfulExit",
            Value::Boolean(false),
        )])),
    );
    values.insert("ExitTimeout".into(), Value::Integer(5.into()));
    values.insert("ThrottleInterval".into(), Value::Integer(5.into()));
    values.insert("Umask".into(), Value::Integer(0o077.into()));
    values.insert("ProcessType".into(), Value::String("Background".into()));
    values.insert(
        "LimitLoadToSessionType".into(),
        Value::String("Aqua".into()),
    );
    Ok(values)
}

fn owns_plist(value: &Dictionary, label: &str) -> bool {
    value.get("Label").and_then(Value::as_string) == Some(label)
        && value
            .get("EnvironmentVariables")
            .and_then(Value::as_dictionary)
            .and_then(|d| d.get("USAGESTAT_MANAGED_SERVICE"))
            .and_then(Value::as_string)
            == Some("1")
}

fn disabled_override(text: &str, label: &str) -> Result<Option<bool>> {
    for line in text.lines() {
        let Some((name, value)) = line.split_once("=>") else {
            continue;
        };
        if name.trim().trim_matches('"') == label {
            return Ok(Some(match value.trim().trim_end_matches(';') {
                "true" | "disabled" => true,
                "false" | "enabled" => false,
                _ => bail!("unexpected launchctl disabled-state output for {label}"),
            }));
        }
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
fn job_dictionary(label: &str) -> Result<Option<Dictionary>> {
    use std::ffi::{CString, c_void};
    type Reference = *const c_void;
    #[link(name = "ServiceManagement", kind = "framework")]
    unsafe extern "C" {
        static kSMDomainUserLaunchd: Reference;
        fn SMJobCopyDictionary(domain: Reference, label: Reference) -> Reference;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: Reference,
            value: *const std::ffi::c_char,
            encoding: u32,
        ) -> Reference;
        fn CFPropertyListCreateData(
            allocator: Reference,
            value: Reference,
            format: isize,
            options: usize,
            error: *mut Reference,
        ) -> Reference;
        fn CFDataGetLength(data: Reference) -> isize;
        fn CFDataGetBytePtr(data: Reference) -> *const u8;
        fn CFRelease(value: Reference);
    }
    struct Owned(Reference);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }
    let label = CString::new(label)?;
    let value = unsafe { CFStringCreateWithCString(std::ptr::null(), label.as_ptr(), 0x08000100) };
    if value.is_null() {
        bail!("create native LaunchAgent label");
    }
    let value = Owned(value);
    let job = unsafe { SMJobCopyDictionary(kSMDomainUserLaunchd, value.0) };
    if job.is_null() {
        return Ok(None);
    }
    let job = Owned(job);
    let data =
        unsafe { CFPropertyListCreateData(std::ptr::null(), job.0, 100, 0, std::ptr::null_mut()) };
    if data.is_null() {
        bail!("serialize native LaunchAgent status");
    }
    let data = Owned(data);
    let length = unsafe { CFDataGetLength(data.0) };
    if !(1..=1_048_576).contains(&length) {
        bail!("unexpected LaunchAgent status size");
    }
    let bytes = unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(data.0), length as usize) };
    Ok(Some(
        Value::from_reader_xml(bytes)?
            .into_dictionary()
            .context("native job status is not a dictionary")?,
    ))
}

#[cfg(not(target_os = "macos"))]
fn job_dictionary(_: &str) -> Result<Option<Dictionary>> {
    bail!("native LaunchAgent status requires macOS")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plist_round_trip_preserves_paths_and_rejects_foreign_agents() {
        let root = std::env::temp_dir().join("launch agent 使用 & \"space\"");
        let installation = Installation {
            owner: root.clone(),
            binary: root.join("usagestatd"),
            bind: "127.0.0.1:7345".parse().unwrap(),
            config: root.join("config.toml"),
            plugin_dirs: vec![root.join("plugins")],
            environment: BTreeMap::new(),
            management_key_file: root.join("key"),
            control_key_file: root.join("control"),
        };
        let value = render_plist(
            "com.usagestat.fixture",
            &installation,
            &root.join("daemon.json"),
            &root.join("logs"),
            true,
        )
        .unwrap();
        let mut bytes = Vec::new();
        Value::Dictionary(value.clone())
            .to_writer_xml(&mut bytes)
            .unwrap();
        let decoded = Value::from_reader_xml(bytes.as_slice())
            .unwrap()
            .into_dictionary()
            .unwrap();
        assert_eq!(decoded, value);
        assert!(owns_plist(&decoded, "com.usagestat.fixture"));
        assert!(!owns_plist(&decoded, "com.other"));
        let mut foreign = decoded.clone();
        foreign.remove("EnvironmentVariables");
        assert!(!owns_plist(&foreign, "com.usagestat.fixture"));
        assert_eq!(decoded["Disabled"].as_boolean(), Some(true));
        assert_eq!(
            decoded["ProgramArguments"].as_array().unwrap()[0].as_string(),
            installation.binary.to_str()
        );
        assert!(
            render_plist(
                "fixture",
                &installation,
                Path::new("relative"),
                &root,
                false
            )
            .is_err()
        );
    }
    #[test]
    fn reads_only_the_requested_native_disabled_override() {
        let text =
            "disabled services = {\n \"other\" => true\n \"com.usagestat.fixture\" => false\n}";
        assert_eq!(
            disabled_override(text, "com.usagestat.fixture").unwrap(),
            Some(false)
        );
        assert_eq!(disabled_override(text, "missing").unwrap(), None);
        for (value, expected) in [("enabled", false), ("disabled", true)] {
            assert_eq!(
                disabled_override(
                    &format!("\"com.usagestat.fixture\" => {value}"),
                    "com.usagestat.fixture"
                )
                .unwrap(),
                Some(expected)
            );
        }
        assert!(
            disabled_override(
                "\"com.usagestat.fixture\" => invalid",
                "com.usagestat.fixture"
            )
            .is_err()
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod live_tests {
    use super::*;
    struct Installed<'a>(&'a LaunchAgent);
    impl Drop for Installed<'_> {
        fn drop(&mut self) {
            let _ = self.0.disable();
            let _ = fs::remove_file(&self.0.file);
            // Remove the disabled override's effect for this now-absent label.
            // No other agent or domain-wide override is changed.
            let _ = launchctl(&["enable", &self.0.target()]);
        }
    }

    #[test]
    #[ignore = "requires a native macOS GUI domain and USAGESTAT_TEST_DAEMON_BINARY"]
    fn isolated_native_launchagent_lifecycle() {
        let binary = PathBuf::from(
            std::env::var_os("USAGESTAT_TEST_DAEMON_BINARY")
                .expect("set the built native daemon path"),
        );
        assert!(binary.is_file());
        let directory = usagestat_core::storage::temporary_directory().unwrap();
        let root = directory.path();
        let label = format!("com.usagestat.native-test.{}", std::process::id());
        let defaults = LaunchAgent::new().unwrap();
        let manager = LaunchAgent {
            file: root.join(format!("{label}.plist")),
            label,
            domain: defaults.domain,
            settings: root.join("daemon.json"),
        };
        manager.validate().unwrap();
        let _cleanup = Installed(&manager);
        let config = root.join("config.toml");
        fs::write(&config, "providers = []\n").unwrap();
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let bind = socket.local_addr().unwrap();
        drop(socket);
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
        settings.save(&manager.settings).unwrap();
        for _ in 0..2 {
            manager.validate().unwrap();
            manager
                .install(settings.installation.as_ref().unwrap(), &manager.settings)
                .unwrap();
            manager.enable().unwrap();
            wait_for_installation(settings.installation.as_ref().unwrap()).unwrap();
            let state = manager.query().unwrap();
            assert!(
                state.registered && state.enabled && state.running,
                "native LaunchAgent did not report running"
            );
        }
        super::lifecycle_tests::independent_controls(&manager, settings.installation.as_ref().unwrap());
        let pid = job_dictionary(&manager.label).unwrap().unwrap()["PID"]
            .as_unsigned_integer()
            .unwrap();
        let mut kill = usagestat_core::process::command("/bin/kill").unwrap();
        kill.args(["-KILL", &pid.to_string()]);
        assert!(
            usagestat_core::process::run(kill, Duration::from_secs(2), 1024)
                .unwrap()
                .status
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let restarted = job_dictionary(&manager.label)
                .unwrap()
                .and_then(|d| d.get("PID").and_then(Value::as_unsigned_integer));
            if restarted.is_some_and(|current| current != pid && current > 0) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "launchd did not restart the crashed owned daemon"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        wait_for_installation(settings.installation.as_ref().unwrap()).unwrap();
        manager.disable().unwrap();
        manager.disable().unwrap();
        let state = manager.query().unwrap();
        assert!(!state.running && !state.enabled);
        assert!(
            !apply_t3(
                &mut settings,
                SavedT3Mode::Auto,
                &manager.settings,
                &key,
                &manager
            )
            .unwrap()
        );
        assert!(!manager.query().unwrap().running);
        let retained = read_key(&key).unwrap();
        manager.enable().unwrap();
        wait_for_installation(settings.installation.as_ref().unwrap()).unwrap();
        assert!(quota_endpoint_available(&local_url(bind), &key));
        manager.set_autostart(false).unwrap();
        assert!(
            apply_t3(
                &mut settings,
                SavedT3Mode::Off,
                &manager.settings,
                &key,
                &manager
            )
            .unwrap()
        );
        wait_for_installation(settings.installation.as_ref().unwrap()).unwrap();
        assert!(!quota_endpoint_available(&local_url(bind), &key));
        assert!(!manager.query().unwrap().enabled);
        manager.set_autostart(true).unwrap();
        assert!(read_key(&key).unwrap() == retained);
        manager.disable().unwrap();
        let moved = root.join("moved installation 使用 space");
        fs::create_dir(&moved).unwrap();
        let moved_binary = moved.join("usagestatd");
        fs::copy(
            &settings.installation.as_ref().unwrap().binary,
            &moved_binary,
        )
        .unwrap();
        settings.installation.as_mut().unwrap().binary = moved_binary;
        settings.save(&manager.settings).unwrap();
        manager
            .install(settings.installation.as_ref().unwrap(), &manager.settings)
            .unwrap();
        manager.enable().unwrap();
        wait_for_installation(settings.installation.as_ref().unwrap()).unwrap();
        manager.disable().unwrap();
        assert!(key.exists() && manager.settings.exists());
        manager.unregister().unwrap();
        manager.unregister().unwrap();
        let removed = manager.query().unwrap();
        assert!(!removed.registered && !removed.running && !removed.enabled);
        assert!(key.exists() && manager.settings.exists());
        let foreign = b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>Label</key><string>unmanaged</string></dict></plist>";
        fs::write(&manager.file, foreign).unwrap();
        assert!(manager.validate().is_err());
        assert!(manager.unregister().is_err());
        assert_eq!(fs::read(&manager.file).unwrap(), foreign);
        // Restore the owned file so Drop can complete its own-label cleanup.
        fs::remove_file(&manager.file).unwrap();
        manager
            .install(settings.installation.as_ref().unwrap(), &manager.settings)
            .unwrap();
    }
}
