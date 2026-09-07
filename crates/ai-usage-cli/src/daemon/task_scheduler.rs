//! Per-user interactive-token tasks, accessed through the native COM API.
use super::*;
use usagestat_core::storage;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, RPC_E_TOO_LATE, VARIANT_BOOL};
use windows::Win32::System::Com::*;
use windows::Win32::System::TaskScheduler::*;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, Interface};

const MARKER: &str = "usagestat managed per-user daemon v1";
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}
struct Session {
    service: ITaskService,
    folder: ITaskFolder,
    // COM interfaces must be released before the apartment.
    _apartment: Apartment,
}
impl Session {
    fn connect() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        let apartment = Apartment;
        static SECURITY: std::sync::OnceLock<std::result::Result<(), String>> =
            std::sync::OnceLock::new();
        SECURITY
            .get_or_init(|| {
                match unsafe {
                    CoInitializeSecurity(
                        None,
                        -1,
                        None,
                        None,
                        RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
                        RPC_C_IMP_LEVEL_IMPERSONATE,
                        None,
                        EOAC_NONE,
                        None,
                    )
                } {
                    Ok(()) => Ok(()),
                    Err(error) if error.code() == RPC_E_TOO_LATE => Ok(()),
                    Err(error) => Err(error.to_string()),
                }
            })
            .as_ref()
            .map_err(|error| anyhow::anyhow!("initialize Task Scheduler security: {error}"))?;
        let service: ITaskService =
            unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }?;
        let empty = VARIANT::default();
        unsafe { service.Connect(&empty, &empty, &empty, &empty) }
            .context("connect to the current user's Task Scheduler")?;
        let folder = unsafe { service.GetFolder(&BSTR::from("\\")) }?;
        Ok(Self {
            service,
            folder,
            _apartment: apartment,
        })
    }
    fn task(&self, name: &str) -> Result<Option<IRegisteredTask>> {
        match unsafe { self.folder.GetTask(&BSTR::from(name)) } {
            Ok(task) => Ok(Some(task)),
            Err(error)
                if error.code() == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) =>
            {
                Ok(None)
            }
            Err(error) => Err(error).context("read the per-user scheduled task"),
        }
    }
}

pub(super) struct ScheduledTask {
    name: String,
    sid: String,
    settings: PathBuf,
}
impl ScheduledTask {
    pub(super) fn new() -> Result<Self> {
        let sid = storage::current_user_sid()?;
        Ok(Self {
            name: format!("{}-{sid}", paths::app_dir_name()),
            sid,
            settings: settings_file()?,
        })
    }
    fn validate_task(&self, task: &IRegisteredTask) -> Result<()> {
        // The description alone is insufficient: verify the principal,
        // saved-profile reference, and the single native executable action.
        unsafe {
            let definition = task.Definition()?;
            let info = definition.RegistrationInfo()?;
            let mut description = BSTR::new();
            let mut source = BSTR::new();
            info.Description(&mut description)?;
            info.Source(&mut source)?;
            let principal = definition.Principal()?;
            let mut user = BSTR::new();
            let mut logon = TASK_LOGON_TYPE::default();
            let mut level = TASK_RUNLEVEL_TYPE::default();
            principal.UserId(&mut user)?;
            principal.LogonType(&mut logon)?;
            principal.RunLevel(&mut level)?;
            let actions = definition.Actions()?;
            let mut count = 0;
            actions.Count(&mut count)?;
            // Task Scheduler can normalize a supplied SID to DOMAIN\user.
            // Resolve that identity back to a SID instead of comparing labels.
            let user = user.to_string();
            for (field, matches) in [
                ("marker", description.to_string() == MARKER),
                ("profile", Path::new(&source.to_string()) == self.settings),
                (
                    "principal",
                    user == self.sid
                        || storage::account_sid(&user).is_ok_and(|sid| sid == self.sid),
                ),
                ("logon type", logon == TASK_LOGON_INTERACTIVE_TOKEN),
                ("run level", level == TASK_RUNLEVEL_LUA),
                ("action count", count == 1),
            ] {
                if !matches {
                    bail!(
                        "refusing to modify unmanaged scheduled task {}: {field} differs",
                        self.name
                    );
                }
            }
            let action: IExecAction = actions
                .get_Item(1)?
                .cast()
                .context("scheduled task has an unmanaged action")?;
            let mut executable = BSTR::new();
            let mut arguments = BSTR::new();
            let mut directory = BSTR::new();
            action.Path(&mut executable)?;
            action.Arguments(&mut arguments)?;
            action.WorkingDirectory(&mut directory)?;
            let executable = PathBuf::from(executable.to_string());
            if !executable.is_absolute()
                || !matches!(
                    executable.file_name().and_then(|s| s.to_str()),
                    Some("usagestat-service.exe" | "usagestat-service-dev.exe")
                )
                || arguments.to_string() != task_arguments(&self.settings)?
                || Path::new(&directory.to_string())
                    != self.settings.parent().context("settings parent")?
            {
                bail!(
                    "refusing to modify unmanaged scheduled task action {}",
                    self.name
                );
            }
        }
        Ok(())
    }
    fn stop_task(&self, task: &IRegisteredTask) -> Result<()> {
        if !task_running(task)? {
            return Ok(());
        }
        // Authenticate the owned backend before asking it to stop. If the
        // endpoint moved or hung, stopping this task closes its private job.
        let graceful = || -> Result<()> {
            let installation = DaemonSettings::load(&self.settings)?
                .and_then(|s| s.installation)
                .context("saved installation")?;
            let health = endpoint(&installation.base_url());
            if !health.healthy || health.owner.as_ref() != Some(&installation.owner) {
                bail!("owned backend is unavailable");
            }
            local_client(Duration::from_secs(2))?
                .post(format!("{}/v1/daemon/shutdown", installation.base_url()))
                .bearer_auth(read_key(&installation.control_key_file)?)
                .send()?
                .error_for_status()?;
            Ok(())
        };
        if graceful().is_ok() && wait_stopped(task, Duration::from_secs(5))? {
            return Ok(());
        }
        unsafe { task.Stop(0) }.context("stop the owned scheduled task")?;
        if !wait_stopped(task, Duration::from_secs(5))? {
            bail!("scheduled task did not stop; inspect {}", self.name);
        }
        Ok(())
    }
}

impl ServiceManager for ScheduledTask {
    fn kind(&self) -> &'static str {
        "task-scheduler"
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn file(&self) -> Option<PathBuf> {
        None
    }
    fn available(&self) -> bool {
        Session::connect().is_ok()
    }
    fn validate(&self) -> Result<()> {
        let session = Session::connect()?;
        if let Some(task) = session.task(&self.name)? {
            self.validate_task(&task)?;
        }
        Ok(())
    }
    fn query(&self) -> Result<Registration> {
        let session = Session::connect()?;
        let Some(task) = session.task(&self.name)? else {
            return Ok(Registration::default());
        };
        self.validate_task(&task)?;
        Ok(Registration {
            registered: true,
            enabled: unsafe { task.Enabled()? }.as_bool(),
            running: task_running(&task)?,
        })
    }
    fn install(&self, installation: &Installation, settings: &Path) -> Result<()> {
        if settings != self.settings {
            bail!("scheduled task settings do not match this profile");
        }
        let launcher = installation.binary.with_file_name(
            if installation
                .binary
                .file_stem()
                .is_some_and(|name| name == "usagestatd-dev")
            {
                "usagestat-service-dev.exe"
            } else {
                "usagestat-service.exe"
            },
        );
        if !launcher.is_file() {
            bail!(
                "Windows background startup requires {} beside the backend; reinstall the complete native package",
                launcher.display()
            );
        }
        let arguments = task_arguments(settings)?;
        let executable = task_path(&launcher)?;
        let directory = task_path(settings.parent().context("settings parent")?)?;
        let session = Session::connect()?;
        let enabled = if let Some(task) = session.task(&self.name)? {
            self.validate_task(&task)?;
            unsafe { task.Enabled()? }.as_bool()
        } else {
            false
        };
        unsafe {
            let definition = session.service.NewTask(0)?;
            let info = definition.RegistrationInfo()?;
            info.SetDescription(&BSTR::from(MARKER))?;
            info.SetSource(&BSTR::from(task_path(settings)?))?;
            let principal = definition.Principal()?;
            principal.SetUserId(&BSTR::from(&self.sid))?;
            principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)?;
            principal.SetRunLevel(TASK_RUNLEVEL_LUA)?;
            let options = definition.Settings()?;
            options.SetEnabled(VARIANT_BOOL::from(enabled))?;
            options.SetAllowDemandStart(VARIANT_BOOL::from(true))?;
            options.SetAllowHardTerminate(VARIANT_BOOL::from(true))?;
            options.SetStartWhenAvailable(VARIANT_BOOL::from(true))?;
            options.SetStopIfGoingOnBatteries(VARIANT_BOOL::from(false))?;
            options.SetDisallowStartIfOnBatteries(VARIANT_BOOL::from(false))?;
            options.SetRunOnlyIfIdle(VARIANT_BOOL::from(false))?;
            options.SetRunOnlyIfNetworkAvailable(VARIANT_BOOL::from(false))?;
            options.SetExecutionTimeLimit(&BSTR::from("PT0S"))?;
            options.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)?;
            options.SetRestartCount(999)?;
            options.SetRestartInterval(&BSTR::from("PT1M"))?;
            let trigger: ILogonTrigger =
                definition.Triggers()?.Create(TASK_TRIGGER_LOGON)?.cast()?;
            trigger.SetUserId(&BSTR::from(&self.sid))?;
            let action: IExecAction = definition.Actions()?.Create(TASK_ACTION_EXEC)?.cast()?;
            action.SetPath(&BSTR::from(executable))?;
            action.SetArguments(&BSTR::from(arguments))?;
            action.SetWorkingDirectory(&BSTR::from(directory))?;
            session
                .folder
                .RegisterTaskDefinition(
                    &BSTR::from(&self.name),
                    &definition,
                    TASK_CREATE_OR_UPDATE.0,
                    &VARIANT::from(self.sid.as_str()),
                    &VARIANT::default(),
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &VARIANT::default(),
                )
                .context("register the current user's login task")?;
        }
        Ok(())
    }
    fn enable(&self) -> Result<()> {
        let session = Session::connect()?;
        let task = session
            .task(&self.name)?
            .context("scheduled task is not installed")?;
        self.validate_task(&task)?;
        self.stop_task(&task)?;
        unsafe {
            task.SetEnabled(VARIANT_BOOL::from(true))?;
            task.Run(&VARIANT::default())?;
        }
        Ok(())
    }
    fn disable(&self) -> Result<()> {
        let session = Session::connect()?;
        if let Some(task) = session.task(&self.name)? {
            self.validate_task(&task)?;
            unsafe { task.SetEnabled(VARIANT_BOOL::from(false)) }?;
            self.stop_task(&task)?;
        }
        Ok(())
    }
    fn unregister(&self) -> Result<()> {
        let session = Session::connect()?;
        if let Some(task) = session.task(&self.name)? {
            self.validate_task(&task)?;
            unsafe { task.SetEnabled(VARIANT_BOOL::from(false)) }?;
            self.stop_task(&task)?;
            self.validate_task(&task)?;
            unsafe { session.folder.DeleteTask(&BSTR::from(&self.name), 0) }
                .context("remove managed per-user scheduled task")?;
        }
        Ok(())
    }
    fn start(&self) -> Result<()> {
        if !self.query()?.running {
            self.restart()?;
        }
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        let session = Session::connect()?;
        if let Some(task) = session.task(&self.name)? {
            self.validate_task(&task)?;
            self.stop_task(&task)?;
        }
        Ok(())
    }
    fn set_autostart(&self, enabled: bool) -> Result<()> {
        let session = Session::connect()?;
        let task = session.task(&self.name)?.context("scheduled task is not registered")?;
        self.validate_task(&task)?;
        unsafe { task.SetEnabled(VARIANT_BOOL::from(enabled)) }?;
        Ok(())
    }
    fn restart(&self) -> Result<()> {
        let session = Session::connect()?;
        let task = session
            .task(&self.name)?
            .context("scheduled task is not installed")?;
        self.validate_task(&task)?;
        self.stop_task(&task)?;
        let enabled = unsafe { task.Enabled()? };
        // Demand-running a disabled task is refused by Windows. Temporarily
        // enable it without changing the user's persisted autostart intent.
        unsafe {
            task.SetEnabled(VARIANT_BOOL::from(true))?;
            let started = task.Run(&VARIANT::default());
            task.SetEnabled(enabled)?;
            started?;
        }
        Ok(())
    }
}

fn task_running(task: &IRegisteredTask) -> Result<bool> {
    Ok(matches!(
        unsafe { task.State()? },
        TASK_STATE_RUNNING | TASK_STATE_QUEUED
    ))
}
fn wait_stopped(task: &IRegisteredTask, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    while task_running(task)? {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(true)
}
fn task_path(path: &Path) -> Result<&str> {
    let text = path
        .to_str()
        .context("Task Scheduler paths must be UTF-8")?;
    if !path.is_absolute() || text.chars().any(|c| c.is_control() || c == '"' || c == '%') {
        bail!(
            "Task Scheduler requires absolute paths without percent signs, quotes, or control characters; choose another installation/config directory"
        );
    }
    Ok(text)
}
fn task_arguments(settings: &Path) -> Result<String> {
    Ok(format!("--service-settings \"{}\"", task_path(settings)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    struct Installed<'a>(&'a ScheduledTask);
    impl Drop for Installed<'_> {
        fn drop(&mut self) {
            let _ = self.0.disable();
            if let Ok(session) = Session::connect() {
                // This guard is only constructed after verifying our fresh,
                // PID-specific test name did not already exist.
                let _ = unsafe { session.folder.DeleteTask(&BSTR::from(&self.0.name), 0) };
            }
            if std::thread::panicking() {
                if let Ok(Some(settings)) = DaemonSettings::load(&self.0.settings) {
                    if let Some(data) = settings
                        .installation
                        .and_then(|i| i.environment.get("USAGESTAT_DATA_DIR").cloned())
                    {
                        for name in ["daemon.stdout.log", "daemon.stderr.log"] {
                            if let Ok(log) =
                                fs::read_to_string(Path::new(&data).join("logs").join(name))
                            {
                                eprintln!("fixture {name}: {log}");
                            }
                        }
                    }
                }
                if let Some(parent) = self.0.settings.parent() {
                    if let Ok(log) = fs::read_to_string(parent.join("daemon-startup-error.log")) {
                        eprintln!("fixture launcher: {log}");
                    }
                }
            }
        }
    }
    #[test]
    fn scheduled_task_paths_preserve_unicode_and_reject_expansion() {
        assert_eq!(
            task_arguments(Path::new(r"C:\Users\使用 & space\daemon.json")).unwrap(),
            r#"--service-settings "C:\Users\使用 & space\daemon.json""#
        );
        for invalid in [
            r"relative\daemon.json",
            r"C:\%USERNAME%\daemon.json",
            "C:\\bad\npath",
        ] {
            assert!(task_path(Path::new(invalid)).is_err());
        }
    }
    fn ready(manager: &ScheduledTask, installation: &Installation) {
        if let Err(error) = wait_for_installation(installation) {
            let session = Session::connect().unwrap();
            let task = session.task(&manager.name).unwrap().unwrap();
            panic!(
                "{error}; native task state={:?}, last result={:?}",
                unsafe { task.State() },
                unsafe { task.LastTaskResult() }
            );
        }
    }

    #[test]
    #[ignore = "requires native interactive Task Scheduler and USAGESTAT_TEST_DAEMON_BINARY"]
    fn isolated_native_scheduled_task_lifecycle() {
        let binary = PathBuf::from(
            std::env::var_os("USAGESTAT_TEST_DAEMON_BINARY").expect("built native daemon path"),
        );
        let directory = storage::temporary_directory().unwrap();
        let root = directory.path().join("profile 使用 & spaces");
        storage::private_directory(&root).unwrap();
        let defaults = ScheduledTask::new().unwrap();
        let manager = ScheduledTask {
            name: format!(
                "usagestat-native-test-{}-{}",
                std::process::id(),
                defaults.sid
            ),
            sid: defaults.sid,
            settings: root.join("daemon.json"),
        };
        assert!(
            Session::connect()
                .unwrap()
                .task(&manager.name)
                .unwrap()
                .is_none()
        );
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
                owner: root.clone(),
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
            ready(&manager, settings.installation.as_ref().unwrap());
            let state = manager.query().unwrap();
            assert!(state.registered && state.enabled && state.running);
        }
        super::lifecycle_tests::independent_controls(&manager, settings.installation.as_ref().unwrap());
        // Crash only this fixture's authenticated, owned backend. The launcher
        // supervisor must restart it without depending on scheduler heuristics.
        let url = settings.installation.as_ref().unwrap().base_url();
        let health: serde_json::Value = local_client(Duration::from_secs(2))
            .unwrap()
            .get(format!("{url}/health"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        let pid = health["pid"].as_u64().unwrap() as u32;
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE, false, pid).unwrap();
            TerminateProcess(process, 37).unwrap();
            CloseHandle(process).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let current = local_client(Duration::from_secs(1))
                .unwrap()
                .get(format!("{url}/health"))
                .send()
                .ok()
                .and_then(|r| r.json::<serde_json::Value>().ok());
            if current.as_ref().is_some_and(|h| {
                h["pid"].as_u64().is_some_and(|p| p != u64::from(pid))
                    && h["owner"].as_str() == root.to_str()
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the scheduled supervisor did not restart the crashed owned backend"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
        manager.disable().unwrap();
        manager.disable().unwrap();
        assert!(!manager.query().unwrap().enabled && !manager.query().unwrap().running);
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
        ready(&manager, settings.installation.as_ref().unwrap());
        assert!(quota_endpoint_available(&url, &key));
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
        ready(&manager, settings.installation.as_ref().unwrap());
        assert!(!quota_endpoint_available(&url, &key));
        assert_eq!(read_key(&key).unwrap(), retained);
        manager.disable().unwrap();
        let moved = root.join("moved installation 使用 & space");
        fs::create_dir(&moved).unwrap();
        let old = &settings.installation.as_ref().unwrap().binary;
        fs::copy(old, moved.join("usagestatd.exe")).unwrap();
        fs::copy(
            old.with_file_name("usagestat-service.exe"),
            moved.join("usagestat-service.exe"),
        )
        .unwrap();
        settings.installation.as_mut().unwrap().binary = moved.join("usagestatd.exe");
        settings.save(&manager.settings).unwrap();
        manager
            .install(settings.installation.as_ref().unwrap(), &manager.settings)
            .unwrap();
        manager.enable().unwrap();
        ready(&manager, settings.installation.as_ref().unwrap());
        manager.disable().unwrap();
        // Make our disposable task foreign, then prove validation and disable
        // preserve it. Cleanup owns this unique name and removes it afterwards.
        manager.unregister().unwrap();
        manager.unregister().unwrap();
        let removed = manager.query().unwrap();
        assert!(!removed.registered && !removed.enabled && !removed.running);
        assert_eq!(read_key(&key).unwrap(), retained);
        manager.install(settings.installation.as_ref().unwrap(), &manager.settings).unwrap();
        let session = Session::connect().unwrap();
        let task = session.task(&manager.name).unwrap().unwrap();
        unsafe {
            let definition = task.Definition().unwrap();
            definition
                .RegistrationInfo()
                .unwrap()
                .SetDescription(&BSTR::from("unrelated fixture task"))
                .unwrap();
            session
                .folder
                .RegisterTaskDefinition(
                    &BSTR::from(&manager.name),
                    &definition,
                    TASK_CREATE_OR_UPDATE.0,
                    &VARIANT::from(manager.sid.as_str()),
                    &VARIANT::default(),
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &VARIANT::default(),
                )
                .unwrap();
        }
        assert!(manager.validate().is_err());
        assert!(manager.disable().is_err());
        assert!(manager.unregister().is_err());
        assert!(session.task(&manager.name).unwrap().is_some());
    }
}
