#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod native;

#[cfg(windows)]
fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--version")) && args.next().is_none() {
        println!("usagestat-service {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if first.as_deref() != Some(std::ffi::OsStr::new("--service-settings")) {
        std::process::exit(2);
    }
    let Some(path) = args.next().map(std::path::PathBuf::from) else {
        std::process::exit(2);
    };
    if args.next().is_some() || !path.is_absolute() {
        std::process::exit(2);
    }
    let code = loop {
        match native::run(&path) {
            Ok(0) => break 0,
            Ok(_) => {
                // Task Scheduler restart-on-failure is not reliable for every
                // demand-start/logon-session combination. Keep the backend's crash
                // recovery local to its supervisor. Every iteration closes the
                // previous job before starting another child tree.
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            Err(error) => {
                if let Some(parent) = path.parent() {
                    let message = format!("Windows background launcher failed: {error}\n");
                    let _ = usagestat_core::storage::append_private(
                        &parent.join("daemon-startup-error.log"),
                        message.as_bytes(),
                    );
                }
                break 1;
            }
        }
    };
    std::process::exit(code as i32);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("usagestat-service is an internal Windows launcher; use usagestatd on this platform");
    std::process::exit(2);
}
