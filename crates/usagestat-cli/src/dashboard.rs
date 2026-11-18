//! Open the native dashboard without changing the daemon's lifecycle or T3 mode.
use anyhow::{Context, Result, bail};
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::daemon;

pub fn run(url_only: bool, bind: Option<SocketAddr>, json: bool) -> Result<()> {
    let base_url = daemon::dashboard_base_url(bind)?;
    let url = format!("{base_url}/dashboard");
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({"dashboardUrl": url}))?
        );
        return Ok(());
    }
    if url_only {
        println!("{url}");
        return Ok(());
    }

    let cli_name = if daemon::dev_profile() {
        "usagestat-dev"
    } else {
        "usagestat"
    };
    let start = if cfg!(target_os = "linux") {
        format!("{cli_name} daemon enable")
    } else if daemon::dev_profile() {
        "usagestatd-dev".to_string()
    } else {
        "usagestatd".to_string()
    };
    daemon::check_health(&base_url).with_context(|| {
        format!("dashboard is not responding at {url}; start the daemon with `{start}`")
    })?;

    open_browser(&url).with_context(|| {
        format!("could not open a browser; open {url} manually or use `{cli_name} dashboard --url`")
    })?;
    println!("Opening {url}");
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        Command::new("xdg-open")
    };
    // Pass the URL as one argument, without evaluating any shell syntax.
    let mut child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("browser launcher failed with {status}");
            }
            return Ok(());
        }
        // Some launchers remain alive until their browser closes. Let the CLI
        // return once the launch request has been handed off.
        if Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
