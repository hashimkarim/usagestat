//! Restart into an upgraded executable.
//!
//! Package managers replace the daemon's file on upgrade, but the running
//! process keeps executing the old one. A service daemon watches its installed
//! path and, once a new working binary has settled there, shuts down normally
//! and re-executes itself in place: same process id, same arguments, the
//! environment it was launched with. No root helper or user-bus access is
//! needed, so it behaves the same for every package format.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How often the installed path is checked: one `stat` per minute.
const INTERVAL: Duration = Duration::from_secs(60);
/// A replacement must stay unchanged this long, so a package transaction that
/// is still writing plugins or other files is not picked up halfway.
const SETTLE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
    len: u64,
    modified_ns: i128,
}

fn identify(path: &Path) -> std::io::Result<FileId> {
    use std::os::unix::fs::MetadataExt;
    // Follows symlinks: an updated link target counts as a replacement.
    let meta = std::fs::metadata(path)?;
    Ok(FileId {
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        modified_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
    })
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Wait,
    Restart,
}

struct Watcher {
    path: PathBuf,
    running: FileId,
    pending: Option<(FileId, Instant)>,
    missing: bool,
    settle: Duration,
}

impl Watcher {
    fn new(path: PathBuf, settle: Duration) -> std::io::Result<Self> {
        Ok(Self {
            running: identify(&path)?,
            path,
            pending: None,
            missing: false,
            settle,
        })
    }

    fn step(&mut self, now: Instant, works: impl Fn(&Path) -> bool) -> Step {
        let current = match identify(&self.path) {
            Ok(current) => current,
            Err(_) => {
                // Uninstalled or mid-swap: keep serving with the running copy.
                if !self.missing {
                    // stderr reaches the service journal without RUST_LOG.
                    eprintln!(
                        "usagestatd: installed daemon {} is missing; continuing with the running version",
                        self.path.display()
                    );
                }
                self.missing = true;
                self.pending = None;
                return Step::Wait;
            }
        };
        self.missing = false;
        if current == self.running {
            self.pending = None;
            return Step::Wait;
        }
        match self.pending {
            Some((seen, since)) if seen == current => {
                if now.duration_since(since) < self.settle {
                    return Step::Wait;
                }
                if works(&self.path) {
                    return Step::Restart;
                }
                eprintln!(
                    "usagestatd: replacement at {} did not report a usagestatd version; keeping the running version",
                    self.path.display()
                );
                // Do not retry the same broken file every minute.
                self.running = current;
                self.pending = None;
                Step::Wait
            }
            _ => {
                self.pending = Some((current, now));
                Step::Wait
            }
        }
    }
}

/// Runs `<path> --version` with a deadline and accepts only a usagestatd.
fn reports_version(path: &Path) -> bool {
    let Ok(mut child) = Command::new(path)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
    let mut text = String::new();
    use std::io::Read;
    let read = child
        .stdout
        .take()
        .map(|mut out| out.by_ref().take(256).read_to_string(&mut text));
    matches!(read, Some(Ok(_))) && text.starts_with("usagestatd ")
}

/// Watches `path` until shutdown. On a settled, working replacement it sets
/// `restart` and then `shutdown`, so the daemon stops through its normal path.
pub fn start(
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    let mut watcher = match Watcher::new(path, SETTLE) {
        Ok(watcher) => watcher,
        Err(error) => {
            eprintln!("usagestatd: upgrade watch disabled: {error}");
            return None;
        }
    };
    Some(thread::spawn(move || {
        let mut next = Instant::now() + INTERVAL;
        while !shutdown.load(Ordering::SeqCst) {
            if Instant::now() < next {
                thread::sleep(Duration::from_secs(1));
                continue;
            }
            next = Instant::now() + INTERVAL;
            if watcher.step(Instant::now(), reports_version) == Step::Restart {
                eprintln!(
                    "usagestatd: installed daemon {} was upgraded; restarting into it",
                    watcher.path.display()
                );
                restart.store(true, Ordering::SeqCst);
                shutdown.store(true, Ordering::SeqCst);
            }
        }
    }))
}

/// Replaces this process with the installed executable. Returns only on error.
pub fn exec(
    path: &Path,
    args: &[std::ffi::OsString],
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    Command::new(path)
        .args(args)
        .env_clear()
        .envs(env.iter().cloned())
        .exec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn replace(path: &Path, text: &str) {
        // Package managers write a new file and rename it into place.
        let staged = path.with_extension("new");
        fs::write(&staged, text).unwrap();
        fs::rename(&staged, path).unwrap();
    }

    #[test]
    fn restarts_only_after_a_working_replacement_settles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usagestatd");
        fs::write(&path, "old").unwrap();
        let mut watcher = Watcher::new(path.clone(), Duration::from_secs(30)).unwrap();
        let t0 = Instant::now();
        assert_eq!(watcher.step(t0, |_| true), Step::Wait, "unchanged");

        replace(&path, "new binary");
        assert_eq!(watcher.step(t0, |_| true), Step::Wait, "just replaced");
        assert_eq!(
            watcher.step(t0 + Duration::from_secs(10), |_| true),
            Step::Wait,
            "still settling"
        );
        // Rewritten again mid-transaction: the settle period starts over.
        replace(&path, "newer binary");
        assert_eq!(
            watcher.step(t0 + Duration::from_secs(40), |_| true),
            Step::Wait
        );
        assert_eq!(
            watcher.step(t0 + Duration::from_secs(80), |_| true),
            Step::Restart
        );
    }

    #[test]
    fn missing_or_broken_replacements_keep_the_running_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usagestatd");
        fs::write(&path, "old").unwrap();
        let mut watcher = Watcher::new(path.clone(), Duration::ZERO).unwrap();
        let t0 = Instant::now();

        fs::remove_file(&path).unwrap();
        assert_eq!(watcher.step(t0, |_| true), Step::Wait);

        replace(&path, "broken");
        assert_eq!(watcher.step(t0, |_| false), Step::Wait);
        assert_eq!(watcher.step(t0, |_| false), Step::Wait, "rejected once");
        // The rejected file is not retried; a later good one still is.
        assert_eq!(watcher.step(t0, |_| true), Step::Wait);
        replace(&path, "fixed");
        assert_eq!(watcher.step(t0, |_| true), Step::Wait);
        assert_eq!(watcher.step(t0, |_| true), Step::Restart);
    }

    #[test]
    fn version_probe_accepts_only_a_usagestat_daemon() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        // A script run right after writing it can hit ETXTBSY while another
        // test thread forks; retry the positive case instead of flaking.
        let good = script("good", "echo 'usagestatd 9.9.9'");
        assert!((0..20).any(|_| reports_version(&good) || {
            thread::sleep(Duration::from_millis(50));
            false
        }));
        assert!(!reports_version(&script("other", "echo 'something 1.0'")));
        assert!(!reports_version(&script(
            "fails",
            "echo 'usagestatd 9.9.9'; exit 1"
        )));
        assert!(!reports_version(&dir.path().join("absent")));
    }
}
