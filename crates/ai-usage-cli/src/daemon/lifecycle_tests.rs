use super::*;
use std::cell::RefCell;

struct FakeManager {
    state: Registration,
    calls: RefCell<Vec<&'static str>>,
    reject: bool,
}
impl ServiceManager for FakeManager {
    fn kind(&self) -> &'static str {
        "fixture"
    }
    fn name(&self) -> String {
        "fixture".into()
    }
    fn file(&self) -> Option<PathBuf> {
        None
    }
    fn validate(&self) -> Result<()> {
        self.calls.borrow_mut().push("validate");
        if self.reject {
            bail!("unmanaged fixture");
        }
        Ok(())
    }
    fn query(&self) -> Result<Registration> {
        self.calls.borrow_mut().push("query");
        Ok(self.state)
    }
    fn install(&self, _: &Installation, _: &Path) -> Result<()> {
        self.calls.borrow_mut().push("install");
        Ok(())
    }
    fn enable(&self) -> Result<()> {
        self.calls.borrow_mut().push("enable");
        Ok(())
    }
    fn disable(&self) -> Result<()> {
        self.calls.borrow_mut().push("disable");
        Ok(())
    }
    fn restart(&self) -> Result<()> {
        self.calls.borrow_mut().push("restart");
        Ok(())
    }
    fn unregister(&self) -> Result<()> {
        self.calls.borrow_mut().push("unregister");
        Ok(())
    }
}

fn installation(root: &Path) -> Installation {
    Installation {
        owner: root.to_owned(),
        binary: root.join("usagestatd"),
        bind: "127.0.0.1:7345".parse().unwrap(),
        config: root.join("config.toml"),
        plugin_dirs: vec![root.join("plugins")],
        environment: BTreeMap::new(),
        management_key_file: root.join("management-key"),
        control_key_file: root.join("control-key"),
    }
}

#[test]
fn t3_persists_without_a_manager_and_keeps_stopped_services_stopped() {
    for (installed, running) in [(false, false), (true, false), (true, true)] {
        let directory = usagestat_core::storage::temporary_directory().unwrap();
        let path = directory.path().join("daemon.json");
        let install = installation(directory.path());
        let key = install.management_key_file.clone();
        let mut settings = DaemonSettings {
            t3_mode: SavedT3Mode::Off,
            installation: installed.then_some(install),
        };
        let manager = FakeManager {
            state: Registration {
                registered: installed,
                running,
                enabled: false,
            },
            calls: RefCell::new(Vec::new()),
            reject: !installed,
        };
        let restarted = apply_t3(&mut settings, SavedT3Mode::Auto, &path, &key, &manager).unwrap();
        assert_eq!(restarted, running);
        let retained_key = read_key(&key).unwrap();
        assert_eq!(
            DaemonSettings::load(&path).unwrap().unwrap().t3_mode,
            SavedT3Mode::Auto
        );
        assert_eq!(
            *manager.calls.borrow(),
            if !installed {
                vec![]
            } else if running {
                vec!["validate", "query", "install", "restart"]
            } else {
                vec!["validate", "query", "install"]
            }
        );
        apply_t3(&mut settings, SavedT3Mode::Off, &path, &key, &manager).unwrap();
        assert!(read_key(&key).unwrap() == retained_key);
        assert_eq!(
            DaemonSettings::load(&path).unwrap().unwrap().t3_mode,
            SavedT3Mode::Off
        );
        assert!(!manager.calls.borrow().contains(&"enable"));
    }
}

#[test]
fn refuses_unmanaged_changes_before_persisting_intent_or_keys() {
    let directory = usagestat_core::storage::temporary_directory().unwrap();
    let path = directory.path().join("daemon.json");
    let install = installation(directory.path());
    let key = install.management_key_file.clone();
    let mut settings = DaemonSettings {
        t3_mode: SavedT3Mode::Off,
        installation: Some(install),
    };
    settings.save(&path).unwrap();
    let manager = FakeManager {
        state: Registration::default(),
        calls: RefCell::new(Vec::new()),
        reject: true,
    };
    assert!(apply_t3(&mut settings, SavedT3Mode::Auto, &path, &key, &manager).is_err());
    assert!(!key.exists());
    assert_eq!(
        DaemonSettings::load(&path).unwrap().unwrap().t3_mode,
        SavedT3Mode::Off
    );
    assert_eq!(*manager.calls.borrow(), ["validate"]);
}

#[test]
fn legacy_boolean_preferences_remain_compatible_and_invalid_settings_fail() {
    let directory = usagestat_core::storage::temporary_directory().unwrap();
    let path = directory.path().join("daemon.json");
    for (text, mode) in [
        (r#"{"t3Enabled":true}"#, SavedT3Mode::Auto),
        (r#"{"t3Enabled":false}"#, SavedT3Mode::Off),
    ] {
        fs::write(&path, text).unwrap();
        assert_eq!(DaemonSettings::load(&path).unwrap().unwrap().t3_mode, mode);
    }
    for text in [
        r#"{"t3Mode":"invalid","t3Enabled":true}"#,
        "{}",
        "invalid json",
    ] {
        fs::write(&path, text).unwrap();
        assert!(DaemonSettings::load(&path).is_err());
    }
}

#[test]
fn unregister_retains_owner_custom_key_paths_t3_intent_and_provider_data() {
    let directory = usagestat_core::storage::temporary_directory().unwrap();
    let root = directory.path();
    let stored = root.join("daemon.json");
    let install = installation(root);
    fs::write(&install.management_key_file, "synthetic retained key").unwrap();
    fs::write(&install.config, "synthetic retained configuration").unwrap();
    let settings = DaemonSettings { t3_mode: SavedT3Mode::Auto, installation: Some(install.clone()) };
    let mut manager = FakeManager {state: Registration::default(), calls: RefCell::new(Vec::new()), reject: true};
    assert!(unregister(&settings, &stored, &manager).is_err());
    assert!(!stored.exists());
    assert_eq!(*manager.calls.borrow(), ["validate"]);
    manager.reject = false;
    manager.calls.borrow_mut().clear();
    unregister(&settings, &stored, &manager).unwrap();
    assert_eq!(*manager.calls.borrow(), ["validate", "unregister"]);
    assert_eq!(DaemonSettings::load(&stored).unwrap(), Some(settings));
    assert_eq!(fs::read_to_string(&install.management_key_file).unwrap(), "synthetic retained key");
    assert_eq!(fs::read_to_string(&install.config).unwrap(), "synthetic retained configuration");
}

// Used by the real, uniquely named native service fixtures on every platform.
// Exercise all running/autostart combinations; installers must restore both.
pub(super) fn independent_controls(manager: &dyn ServiceManager, install: &Installation) {
    let state = || {
        let state = manager.query().unwrap();
        assert!(state.registered);
        (state.running, state.enabled)
    };
    let assert_ready = || wait_for_installation(install).unwrap();
    assert_ready();
    assert_eq!(state(), (true, true));
    manager.set_autostart(false).unwrap();
    manager.set_autostart(false).unwrap();
    assert_eq!(state(), (true, false));
    assert_ready();
    manager.stop().unwrap();
    manager.stop().unwrap();
    assert_eq!(state(), (false, false));
    manager.start().unwrap();
    assert_ready();
    manager.start().unwrap();
    assert_eq!(state(), (true, false));
    manager.stop().unwrap();
    manager.set_autostart(true).unwrap();
    manager.set_autostart(true).unwrap();
    assert_eq!(state(), (false, true));
    manager.start().unwrap();
    assert_ready();
    assert_eq!(state(), (true, true));
    manager.stop().unwrap();
    assert_eq!(state(), (false, true));
    manager.set_autostart(false).unwrap();
    assert_eq!(state(), (false, false));
    manager.enable().unwrap();
    assert_ready();
    assert_eq!(state(), (true, true));
}
