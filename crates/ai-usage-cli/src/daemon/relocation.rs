//! Move a registered installation between retained package versions, keeping
//! the saved profile and independent running/login preferences recoverable.
use super::*;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    schema_version: u8,
    before: Installation,
    after: Installation,
    t3_mode: SavedT3Mode,
    running: bool,
    autostart: bool,
    old_version: String,
    old_binary_sha256: String,
    committed: bool,
}

pub(super) fn journal_path(settings: &Path) -> PathBuf {
    settings.with_file_name("daemon-relocation.json")
}

fn binary_sha256(binary: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = fs::File::open(binary).context("open the retained previous daemon")?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn binary_version(binary: &Path) -> Result<String> {
    let mut command = usagestat_core::process::command(binary)?;
    command.arg("--version");
    let output = usagestat_core::process::run(command, Duration::from_secs(15), 4096)?;
    anyhow::ensure!(output.status.success(), "the selected daemon did not report its version");
    let text = String::from_utf8(output.stdout)?;
    let version = text.trim().strip_prefix("usagestatd ").context("unexpected daemon version response")?;
    anyhow::ensure!(!version.is_empty() && version.bytes().all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b)), "invalid daemon version response");
    Ok(version.to_owned())
}

fn saved(journal: &Journal, installation: &Installation) -> DaemonSettings {
    DaemonSettings { t3_mode: journal.t3_mode, installation: Some(installation.clone()) }
}

pub(super) fn relocate(settings: &mut DaemonSettings, path: &Path, binary: Option<&Path>, manager: &dyn ServiceManager) -> Result<()> {
    manager.validate()?;
    let state = manager.query()?;
    anyhow::ensure!(state.registered && path.is_file(), "relocation requires an existing managed registration; use daemon enable for first installation");
    let before = settings.installation.as_ref().context("no saved installation")?.clone();
    let binary = find_binary(binary)?;
    anyhow::ensure!(installation_owner(&binary)? == before.owner, "the selected daemon belongs to another installation; use daemon enable --switch-owner for an explicit transfer");
    anyhow::ensure!(binary_version(&binary)? == env!("CARGO_PKG_VERSION"), "the selected daemon and CLI versions must match");
    let health = endpoint(&before.base_url());
    anyhow::ensure!(!health.occupied || (state.running && health.healthy && health.owner.as_ref() == Some(&before.owner)),
        "preserve the conflicting or foreground process before relocating the managed daemon");
    if before.binary == binary {
        anyhow::ensure!(!state.running || health.version.as_deref() == Some(env!("CARGO_PKG_VERSION")),
            "the executable was replaced in place; use that installer's restart/recovery procedure instead of path relocation");
        return Ok(());
    }
    // Package managers must retain the previous executable until relocation
    // commits. Otherwise recovery would only restore a path to a removed file.
    let old_version = binary_version(&before.binary).context("retain or restore the previous package version before relocating")?;
    let old_binary_sha256 = binary_sha256(&before.binary)?;
    let mut after = before.clone();
    after.plugin_dirs = paths::relocate_installed_plugin_dirs(&before.plugin_dirs, &before.binary, &binary, paths::app_dir_name());
    after.binary = binary;
    let mut journal = Journal { schema_version: 1, before, after, t3_mode: settings.t3_mode,
        running: state.running, autostart: state.enabled, old_version, old_binary_sha256, committed: false };
    let pending = journal_path(path);
    anyhow::ensure!(usagestat_core::storage::create_once(&pending, &serde_json::to_vec_pretty(&journal)?)?,
        "a previous relocation needs daemon recover before continuing");
    let changed = (|| -> Result<()> {
        manager.set_autostart(false)?;
        manager.stop()?;
        let next = saved(&journal, &journal.after);
        next.save(path)?;
        manager.install(&journal.after, path)?;
        manager.set_autostart(journal.autostart)?;
        if journal.running {
            manager.start()?;
            wait_for_installation(&journal.after)?;
        }
        journal.committed = true;
        usagestat_core::storage::write_atomic(&pending, &serde_json::to_vec_pretty(&journal)?)?;
        *settings = next;
        fs::remove_file(&pending).context("remove the committed relocation record")?;
        Ok(())
    })();
    if let Err(error) = changed {
        match recover(path, manager) {
            Ok(()) => {
                *settings = DaemonSettings::load(path)?.context("restored daemon settings")?;
                return Err(error.context("relocation did not complete; the recorded installation state was recovered"));
            }
            Err(recovery) => bail!("relocation failed: {error:#}; recovery remains pending: {recovery:#}; retain the previous package and run daemon recover"),
        }
    }
    Ok(())
}

pub(super) fn recover(path: &Path, manager: &dyn ServiceManager) -> Result<()> {
    let pending = journal_path(path);
    if !pending.try_exists()? { return Ok(()); }
    let journal: Journal = serde_json::from_str(&usagestat_core::storage::read_private(&pending)?)?;
    anyhow::ensure!(journal.schema_version == 1 && journal.before.owner == journal.after.owner, "unknown relocation record; preserve it for manual recovery");
    let current = DaemonSettings::load(path)?.context("saved settings disappeared during relocation")?;
    anyhow::ensure!(current == saved(&journal, &journal.before) || current == saved(&journal, &journal.after),
        "daemon settings changed outside the relocation; preserve them for manual recovery");
    if journal.committed {
        anyhow::ensure!(current == saved(&journal, &journal.after), "committed relocation no longer matches the saved installation");
        fs::remove_file(pending)?;
        return Ok(());
    }
    anyhow::ensure!(binary_sha256(&journal.before.binary)? == journal.old_binary_sha256,
        "the retained previous daemon changed; restore its recorded contents before recovery");
    anyhow::ensure!(binary_version(&journal.before.binary)? == journal.old_version,
        "restore the retained previous daemon version before recovery");
    manager.validate()?;
    anyhow::ensure!(manager.query()?.registered, "the managed registration disappeared; preserve the relocation record for manual recovery");
    manager.set_autostart(false)?;
    manager.stop()?;
    saved(&journal, &journal.before).save(path)?;
    manager.install(&journal.before, path)?;
    manager.set_autostart(journal.autostart)?;
    if journal.running {
        manager.start()?;
        wait_for_installation_version(&journal.before, &journal.old_version)?;
    }
    fs::remove_file(pending)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        state: Cell<Registration>,
        fail_install: Cell<bool>,
        calls: RefCell<Vec<&'static str>>,
    }
    impl ServiceManager for Fixture {
        fn kind(&self) -> &'static str { "fixture" }
        fn name(&self) -> String { "fixture".into() }
        fn file(&self) -> Option<PathBuf> { None }
        fn validate(&self) -> Result<()> { self.calls.borrow_mut().push("validate"); Ok(()) }
        fn query(&self) -> Result<Registration> { Ok(self.state.get()) }
        fn install(&self, _: &Installation, _: &Path) -> Result<()> {
            self.calls.borrow_mut().push("install");
            anyhow::ensure!(!self.fail_install.replace(false), "synthetic registration replacement failure");
            Ok(())
        }
        fn enable(&self) -> Result<()> { bail!("combined enable must not be used") }
        fn disable(&self) -> Result<()> { bail!("combined disable must not be used") }
        fn restart(&self) -> Result<()> { bail!("restart must not start a paused installation") }
        fn stop(&self) -> Result<()> { let mut state = self.state.get(); state.running = false; self.state.set(state); Ok(()) }
        fn start(&self) -> Result<()> { bail!("the paused fixture must not be started") }
        fn set_autostart(&self, enabled: bool) -> Result<()> { let mut state = self.state.get(); state.enabled = enabled; self.state.set(state); Ok(()) }
    }

    #[test]
    fn failed_registration_and_interrupted_relocation_restore_paused_intent_and_saved_paths() {
        let temporary = usagestat_core::storage::temporary_directory().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let owner = root.join("Cellar/usagestat");
        let old = owner.join("1.0.0/bin/usagestatd");
        let new = owner.join("1.0.1/bin/usagestatd");
        for (binary, version) in [(&old, "0.9.0"), (&new, env!("CARGO_PKG_VERSION"))] {
            fs::create_dir_all(binary.parent().unwrap()).unwrap();
            fs::write(binary, format!("#!/bin/sh\nprintf 'usagestatd {version}\\n'\n")).unwrap();
            fs::set_permissions(binary, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let bind = socket.local_addr().unwrap();
        drop(socket);
        let before = Installation { owner, binary: old, bind, config: root.join("custom.toml"),
            plugin_dirs: vec![root.join("custom plugins"), root.join("Cellar/usagestat/1.0.0/share/usagestat/plugins")],
            environment: BTreeMap::from([("CODEX_HOME".into(), root.join("custom codex").to_str().unwrap().into())]),
            management_key_file: root.join("custom t3 key"), control_key_file: root.join("custom control key") };
        let original = DaemonSettings { t3_mode: SavedT3Mode::Auto, installation: Some(before.clone()) };
        let path = root.join("daemon.json");
        original.save(&path).unwrap();
        let manager = Fixture { state: Cell::new(Registration { registered: true, enabled: true, running: false }),
            fail_install: Cell::new(true), calls: RefCell::new(vec![]) };
        let mut settings = original.clone();
        assert!(relocate(&mut settings, &path, Some(&new), &manager).unwrap_err().to_string().contains("recovered"));
        assert_eq!(DaemonSettings::load(&path).unwrap().unwrap(), original);
        assert!(manager.state.get().enabled && !manager.state.get().running);
        assert!(!journal_path(&path).exists());

        let mut after = before.clone(); after.binary = new;
        let old_binary_sha256 = binary_sha256(&before.binary).unwrap();
        let journal = Journal { schema_version: 1, before, after, t3_mode: SavedT3Mode::Auto,
            running: false, autostart: true, old_version: "0.9.0".into(), old_binary_sha256, committed: false };
        usagestat_core::storage::write_atomic(&journal_path(&path), &serde_json::to_vec(&journal).unwrap()).unwrap();
        let mut changed = saved(&journal, &journal.after);
        changed.t3_mode = SavedT3Mode::Off;
        changed.save(&path).unwrap();
        manager.calls.borrow_mut().clear();
        assert!(recover(&path, &manager).is_err());
        assert!(manager.calls.borrow().is_empty());
        assert_eq!(DaemonSettings::load(&path).unwrap().unwrap(), changed);
        saved(&journal, &journal.after).save(&path).unwrap();
        let original_binary = fs::read(&journal.before.binary).unwrap();
        let mut modified_binary = original_binary.clone();
        modified_binary.extend_from_slice(b"\n# Replaced payload with the same reported version\n");
        fs::write(&journal.before.binary, modified_binary).unwrap();
        assert_eq!(binary_version(&journal.before.binary).unwrap(), journal.old_version);
        assert!(recover(&path, &manager).unwrap_err().to_string().contains("recorded contents"));
        assert!(manager.calls.borrow().is_empty());
        assert_eq!(DaemonSettings::load(&path).unwrap().unwrap(), saved(&journal, &journal.after));
        fs::write(&journal.before.binary, original_binary).unwrap();
        recover(&path, &manager).unwrap();
        recover(&path, &manager).unwrap();
        assert_eq!(DaemonSettings::load(&path).unwrap().unwrap(), original);
        assert!(manager.state.get().enabled && !manager.state.get().running);
    }
}
