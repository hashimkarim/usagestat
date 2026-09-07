use super::*;

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Registration {
    pub registered: bool,
    pub enabled: bool,
    pub running: bool,
}

pub(super) trait ServiceManager {
    fn kind(&self) -> &'static str;
    fn name(&self) -> String;
    fn file(&self) -> Option<PathBuf>;
    fn available(&self) -> bool {
        true
    }
    fn validate(&self) -> Result<()>;
    fn migrate(&self) -> Result<Option<DaemonSettings>> {
        Ok(None)
    }
    fn query(&self) -> Result<Registration>;
    fn install(&self, installation: &Installation, settings: &Path) -> Result<()>;
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn start(&self) -> Result<()> {
        bail!("this service adapter cannot start independently of login startup")
    }
    fn stop(&self) -> Result<()> {
        bail!("this service adapter cannot stop independently of login startup")
    }
    fn set_autostart(&self, _enabled: bool) -> Result<()> {
        bail!("this service adapter cannot change login startup independently")
    }
    fn unregister(&self) -> Result<()> {
        bail!("this service adapter cannot remove login registration")
    }
    fn restart(&self) -> Result<()>;
}

pub(super) fn native() -> Result<Box<dyn ServiceManager>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(super::systemd::Systemd::new()?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(super::launchd::LaunchAgent::new()?))
    }
    #[cfg(windows)]
    {
        Ok(Box::new(super::task_scheduler::ScheduledTask::new()?))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Ok(Box::new(Unavailable))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
struct Unavailable;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl ServiceManager for Unavailable {
    fn kind(&self) -> &'static str {
        "none"
    }
    fn name(&self) -> String {
        paths::app_dir_name().to_owned()
    }
    fn file(&self) -> Option<PathBuf> {
        None
    }
    fn available(&self) -> bool {
        false
    }
    fn validate(&self) -> Result<()> {
        bail!(
            "native autostart is not installed for this platform; run usagestatd in the foreground"
        )
    }
    fn query(&self) -> Result<Registration> {
        Ok(Registration::default())
    }
    fn install(&self, _: &Installation, _: &Path) -> Result<()> {
        self.validate()
    }
    fn enable(&self) -> Result<()> {
        self.validate()
    }
    fn disable(&self) -> Result<()> {
        self.validate()
    }
    fn restart(&self) -> Result<()> {
        self.validate()
    }
}
