//! Shared process shutdown notification. Call explicitly at a binary's entry point.
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static REGISTERED: OnceLock<io::Result<()>> = OnceLock::new();

pub fn register() -> io::Result<Arc<AtomicBool>> {
    let flag = FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)));
    let result = REGISTERED.get_or_init(|| {
        #[cfg(unix)]
        {
            use signal_hook::{
                consts::signal::{SIGINT, SIGTERM},
                flag as signal_flag,
            };
            signal_flag::register(SIGINT, flag.clone())?;
            signal_flag::register(SIGTERM, flag.clone())?;
        }
        #[cfg(windows)]
        unsafe {
            use windows::Win32::System::Console::SetConsoleCtrlHandler;
            SetConsoleCtrlHandler(Some(console_handler), true)
                .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xffff))?;
            // CREATE_NEW_PROCESS_GROUP can inherit Ctrl+C suppression. Restore
            // processing for this process after registering its native handler.
            SetConsoleCtrlHandler(None, false)
                .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xffff))?;
        }
        Ok(())
    });
    result
        .as_ref()
        .map(|_| flag.clone())
        .map_err(|error| io::Error::new(error.kind(), error.to_string()))
}

pub(crate) fn current() -> Option<Arc<AtomicBool>> {
    FLAG.get().cloned()
}

#[cfg(windows)]
unsafe extern "system" fn console_handler(event: u32) -> windows::core::BOOL {
    use std::sync::atomic::Ordering;
    use windows::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    if !matches!(
        event,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) {
        return false.into();
    }
    if let Some(flag) = FLAG.get() {
        flag.store(true, Ordering::SeqCst);
    }
    if matches!(
        event,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        // Windows ends the process when this callback returns. Keep its thread
        // alive briefly so the main/probe threads can terminate owned helpers.
        // No console I/O is performed here. The ordinary close budget is 5 s.
        std::thread::sleep(std::time::Duration::from_secs(4));
    }
    true.into()
}
