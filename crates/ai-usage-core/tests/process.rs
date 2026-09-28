use std::ffi::OsStr;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};
use usagestat_core::process::{self, CancellationToken};

fn helper() -> &'static Path {
    static HELPER: OnceLock<PathBuf> = OnceLock::new();
    HELPER.get_or_init(|| {
        let directory =
            std::env::temp_dir().join(format!("usagestat-process-helper-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let binary = directory.join(format!("helper{}", std::env::consts::EXE_SUFFIX));
        let compiler = Command::new("rustc")
            .args(["--edition=2024", "--crate-name=process_fixture"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/process_helper.rs"))
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compiler.status.success(),
            "{}",
            String::from_utf8_lossy(&compiler.stderr)
        );
        binary
    })
}

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "usagestat process 使用 {}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(mode: &str) -> Command {
    let mut command = process::command(helper()).unwrap();
    command.arg(mode);
    command
}

#[test]
fn native_arguments_and_exit_status_survive_without_shell_interpretation() {
    let arguments = [
        "",
        "with spaces",
        "使用 café",
        "quote\"inside",
        "trailing\\",
        "& | < > ^ %PATH% !VAR! $(echo no)",
        "line\nbreak",
    ];
    let mut command = fixture("echo");
    command.args(arguments);
    let output = process::run(command, Duration::from_secs(5), 8192).unwrap();
    assert!(output.status.success());
    let expected = arguments
        .iter()
        .flat_map(|arg| arg.as_bytes().iter().copied().chain([0]))
        .collect::<Vec<_>>();
    assert_eq!(output.stdout, expected);
    assert_eq!(output.stderr, b"fixture stderr");
    let mut command = fixture("exit");
    command.arg("7");
    assert_eq!(
        process::run(command, Duration::from_secs(5), 100)
            .unwrap()
            .status
            .code(),
        Some(7)
    );
}

#[test]
fn output_is_capped_while_both_pipes_are_drained_and_stdin_is_closed() {
    let output = process::run(fixture("output"), Duration::from_secs(10), 1003).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, vec![b'o'; 1003]);
    assert_eq!(output.stderr, vec![b'e'; 1003]);
    assert!(
        process::run(fixture("stdin"), Duration::from_secs(5), 10)
            .unwrap()
            .status
            .success()
    );
    assert_eq!(process::decode_output(&[b'a', 0xe2, 0x82]), "a");
}

#[cfg(unix)]
#[test]
fn input_is_bounded_and_output_cannot_deadlock_the_writer() {
    let input = vec![b'i'; 131072];
    let output =
        process::run_with_input(fixture("input"), Duration::from_secs(5), 100, &input).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 100);
    assert_eq!(output.stderr.len(), 100);
    let started = Instant::now();
    let error = process::run_with_input(
        fixture("ignore-input"),
        Duration::from_millis(200),
        100,
        &input,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn helper_resolution_works_with_only_an_explicit_background_path() {
    let directory = TestDir::new();
    let binary = directory
        .0
        .join(format!("gh{}", std::env::consts::EXE_SUFFIX));
    // Reuse the already-closed executable. Copying while other tests fork can
    // briefly inherit its writable descriptor and make exec fail with ETXTBSY.
    #[cfg(unix)]
    std::os::unix::fs::symlink(helper(), &binary).unwrap();
    #[cfg(windows)]
    std::fs::copy(helper(), &binary).unwrap();
    let search = std::env::join_paths([&directory.0]).unwrap();
    let mut command = process::command_with_path(OsStr::new("gh"), Some(&search)).unwrap();
    command.args(["echo", "background 使用"]);
    let output = process::run(command, Duration::from_secs(5), 100).unwrap();
    assert_eq!(output.stdout, "background 使用\0".as_bytes());
    assert!(process::resolve_executable(OsStr::new("missing"), Some(&search)).is_err());
    assert!(process::resolve_executable(OsStr::new("gh"), None).is_err());
}

fn wait_ready(path: &Path) -> SocketAddr {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(address) = text.parse() {
                return address;
            }
        }
        assert!(Instant::now() < deadline, "descendant did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_stopped(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
        assert!(Instant::now() < deadline, "descendant survived cleanup");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn timeout_kills_descendants_that_hold_output_pipes() {
    let directory = TestDir::new();
    let ready = directory.0.join("ready");
    let mut command = fixture("tree");
    command.arg(&ready);
    let start = Instant::now();
    let error = process::run(command, Duration::from_secs(2), 100).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_stopped(wait_ready(&ready));
}

#[test]
fn cancellation_kills_the_tree_and_returns_promptly() {
    let directory = TestDir::new();
    let ready = directory.0.join("ready");
    let mut command = fixture("tree");
    command.arg(&ready);
    let token = CancellationToken::default();
    let worker_token = token.clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = tx.send(process::with_cancellation(worker_token, || {
            process::run(command, Duration::from_secs(30), 100)
        }));
    });
    let address = wait_ready(&ready);
    token.cancel();
    let error = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("cancellation hung")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    worker.join().unwrap();
    assert_stopped(address);
}

#[test]
fn normal_parent_exit_cleans_up_a_surviving_descendant() {
    let directory = TestDir::new();
    let ready = directory.0.join("ready");
    let mut command = fixture("exit-tree");
    command.arg(&ready);
    let start = Instant::now();
    assert!(
        process::run(command, Duration::from_secs(5), 100)
            .unwrap()
            .status
            .success()
    );
    assert!(start.elapsed() < Duration::from_secs(4));
    assert_stopped(wait_ready(&ready));
}

#[test]
fn cancelled_scope_never_starts_a_child_and_restores_the_previous_scope() {
    let token = CancellationToken::default();
    token.cancel();
    process::with_cancellation(token, || {
        assert_eq!(
            process::run(fixture("stdin"), Duration::from_secs(1), 10)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    });
    assert!(process::current_cancellation().is_none());
}

#[cfg(unix)]
#[test]
fn control_dialogue_waits_for_initialization_and_finishes_without_stdin_eof() {
    let mut output = Vec::new();
    let mut initialized = false;
    let start = Instant::now();
    let status = process::control_dialogue(
        fixture("dialogue"),
        b"initialize\n",
        Duration::from_secs(3),
        4096,
        |bytes| {
            output.extend_from_slice(bytes);
            if output.ends_with(b"quota\n") {
                return Ok(process::StreamControl::Finish);
            }
            if !initialized && output.ends_with(b"initialized\n") {
                initialized = true;
                return Ok(process::StreamControl::Reply(b"get_usage\n".to_vec()));
            }
            Ok(process::StreamControl::Continue)
        },
    )
    .unwrap();
    assert!(status.is_none());
    assert_eq!(output, b"initialized\nquota\n");
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[cfg(unix)]
#[test]
fn control_dialogue_caps_output_and_honors_cancellation() {
    let error =
        process::control_dialogue(fixture("output"), b"", Duration::from_secs(3), 4096, |_| {
            Ok(process::StreamControl::Continue)
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let token = CancellationToken::default();
    token.cancel();
    process::with_cancellation(token, || {
        assert_eq!(
            process::control_dialogue(
                fixture("ignore-input"),
                b"init",
                Duration::from_secs(30),
                4096,
                |_| Ok(process::StreamControl::Continue)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
    });
}

#[cfg(unix)]
#[test]
fn control_dialogue_timeout_terminates_descendants() {
    let directory = TestDir::new();
    let ready = directory.0.join("dialogue-ready");
    let mut command = fixture("tree");
    command.arg(&ready);
    let error = process::control_dialogue(command, b"init", Duration::from_secs(2), 4096, |_| {
        Ok(process::StreamControl::Continue)
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_stopped(wait_ready(&ready));
}

#[cfg(windows)]
#[test]
fn npm_shims_use_node_and_preserve_quotes_and_metacharacters() {
    let directory = TestDir::new();
    let entry = directory.0.join("node_modules/npm/bin/npm-cli.js");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(
        &entry,
        "process.stdout.write(JSON.stringify(process.argv.slice(2)))",
    )
    .unwrap();
    std::fs::write(
        directory.0.join("npm.cmd"),
        "@echo unexpected batch execution\r\nexit /b 99\r\n",
    )
    .unwrap();
    let search = std::env::join_paths(
        std::iter::once(directory.0.clone())
            .chain(std::env::split_paths(&process::helper_path().unwrap())),
    )
    .unwrap();
    let mut command = process::command_with_path(OsStr::new("npm"), Some(&search)).unwrap();
    let arguments = [
        "with spaces",
        "使用",
        "quote\"inside",
        "& whoami | echo ^ %PATH% !VAR!",
        "",
    ];
    command.args(arguments);
    // This checks argv fidelity. Cold Node startup and Windows image scanning
    // can exceed five seconds on a busy release runner. Dedicated tests retain
    // strict timeout and cancellation bounds.
    let output = process::run(command, Duration::from_secs(30), 4096).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&output.stdout).unwrap(),
        arguments
    );
}

#[cfg(windows)]
#[test]
fn generic_batch_shims_accept_safe_arguments_and_reject_shell_syntax() {
    let directory = TestDir::new();
    let shim = directory.0.join("firectl.cmd");
    std::fs::write(
        &shim,
        format!("@echo off\r\n\"{}\" echo %*\r\n", helper().display()),
    )
    .unwrap();
    let mut command = process::command(&shim).unwrap();
    command.args(["with spaces", "使用", "--flag=value"]);
    assert_eq!(
        process::run(command, Duration::from_secs(5), 4096)
            .unwrap()
            .stdout,
        "with spaces\0使用\0--flag=value\0".as_bytes()
    );
    for argument in ["quote\"inside", "& echo injection", "%PATH%", "line\nbreak"] {
        let mut command = process::command(&shim).unwrap();
        command.arg(argument);
        assert_eq!(
            process::run(command, Duration::from_secs(5), 100)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
