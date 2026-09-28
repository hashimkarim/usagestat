//! Synchronous helpers with native tree cleanup and bounded, nonblocking output.
//! Commands are resolved before spawning; provider arguments never form shell code.

use process_wrap::std::{ChildWrapper, CommandWrap};
use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(10);
const CLEANUP: Duration = Duration::from_secs(1);

#[derive(Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    interrupt: Option<Arc<AtomicBool>>,
}

impl CancellationToken {
    pub fn with_interrupt(interrupt: Option<Arc<AtomicBool>>) -> Self {
        Self {
            interrupt,
            ..Self::default()
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
            || self
                .interrupt
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }
}

thread_local! {
    static CANCELLATION: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
}

pub fn current_cancellation() -> Option<CancellationToken> {
    CANCELLATION.with(|value| value.borrow().clone())
}

/// Share cancellation through a synchronous probe's nested host calls. Restore
/// the previous scope on return or panic; tokens never leak between providers.
pub fn with_cancellation<T>(token: CancellationToken, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<CancellationToken>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CANCELLATION.with(|value| *value.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CANCELLATION.with(|value| value.replace(Some(token))));
    run()
}

pub fn command(program: impl AsRef<OsStr>) -> io::Result<Command> {
    command_with_path(program.as_ref(), helper_path().as_deref())
}

pub fn helper_path() -> Option<std::ffi::OsString> {
    std::env::var_os("USAGESTAT_HELPER_PATH")
        .filter(|path| !path.is_empty())
        .or_else(|| std::env::var_os("PATH"))
}

pub fn command_with_path(program: &OsStr, search_path: Option<&OsStr>) -> io::Result<Command> {
    let executable = resolve_executable(program, search_path)?;
    #[cfg(windows)]
    let mut command =
        npm_shim_command(&executable, search_path)?.unwrap_or_else(|| Command::new(&executable));
    #[cfg(not(windows))]
    let mut command = Command::new(executable);
    if let Some(path) = search_path {
        command.env("PATH", path);
    }
    Ok(command)
}

/// Search only the supplied PATH or an explicit path. Do not add the current
/// directory implicitly, and do not require a shell's executable lookup.
pub fn resolve_executable(program: &OsStr, search_path: Option<&OsStr>) -> io::Result<PathBuf> {
    let program = Path::new(program);
    let roots: Vec<PathBuf> = if program.is_absolute() || program.components().count() > 1 {
        vec![program.to_owned()]
    } else {
        search_path
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.join(program))
            .collect()
    };
    for root in roots {
        for candidate in executable_candidates(root) {
            if is_executable(&candidate) {
                // Keep Windows paths out of extended-length syntax: cmd cannot
                // handle all canonical \\?\ paths. Relative paths become absolute.
                return std::path::absolute(candidate);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "helper {} was not found in the configured PATH",
            program.display()
        ),
    ))
}

fn executable_candidates(path: PathBuf) -> Vec<PathBuf> {
    #[cfg(windows)]
    if path.extension().is_none() {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.COM;.CMD;.BAT".into());
        return extensions
            .split(';')
            .filter_map(|extension| {
                let extension = extension.trim().to_ascii_lowercase();
                matches!(extension.as_str(), ".exe" | ".com" | ".cmd" | ".bat")
                    .then(|| path.with_extension(&extension[1..]))
            })
            .collect();
    }
    vec![path]
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(windows)]
fn npm_shim_command(path: &Path, search_path: Option<&OsStr>) -> io::Result<Option<Command>> {
    if !is_batch(path) {
        return Ok(None);
    }
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let entries: &[&str] = match name.as_str() {
        "npm" => &["node_modules/npm/bin/npm-cli.js"],
        "npx" => &["node_modules/npm/bin/npx-cli.js"],
        "pnpm" => &[
            "node_modules/pnpm/bin/pnpm.cjs",
            "node_modules/corepack/dist/pnpm.js",
        ],
        "yarn" => &[
            "node_modules/yarn/bin/yarn.js",
            "node_modules/corepack/dist/yarn.js",
        ],
        _ => return Ok(None),
    };
    let parent = path.parent().expect("resolved executable has a parent");
    for entry in entries {
        let entry = parent.join(entry);
        if entry.is_file() {
            let local_node = parent.join("node.exe");
            let node = if local_node.is_file() {
                local_node
            } else {
                resolve_executable(OsStr::new("node"), search_path)?
            };
            let mut command = Command::new(node);
            command.arg(entry);
            return Ok(Some(command));
        }
    }
    Ok(None)
}

#[cfg(windows)]
fn is_batch(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    })
}

fn validate_arguments(command: &Command) -> io::Result<()> {
    #[cfg(windows)]
    if is_batch(Path::new(command.get_program())) {
        // Known npm shims use their Node entry point and accept native argv.
        // Other batch files receive only this conservative cmd-safe subset;
        // Rust handles quoting. Never concatenate a cmd /c command string.
        for value in std::iter::once(command.get_program()).chain(command.get_args()) {
            if value
                .to_string_lossy()
                .chars()
                .any(|c| c.is_control() || "\"%&|<>^!".contains(c))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "batch helper argument cannot be represented safely; use a native executable or a supported npm Node entry point",
                ));
            }
        }
    }
    let _ = command;
    Ok(())
}

/// Capture each stream up to `output_limit` bytes while draining excess bytes.
/// The child and its ordinary descendants are terminated on cancellation,
/// timeout, errors, and parent exit. No reader threads can outlive this call.
pub fn run(command: Command, timeout: Duration, output_limit: usize) -> io::Result<Output> {
    run_inner(command, timeout, output_limit, None)
}

/// Feed a Unix helper through a nonblocking pipe while draining both output
/// streams. Used for Secret Service writes; secret input never enters argv or
/// a temporary file. Native Windows credential APIs do not need this adapter.
#[cfg(unix)]
pub fn run_with_input(
    command: Command,
    timeout: Duration,
    output_limit: usize,
    input: &[u8],
) -> io::Result<Output> {
    run_inner(command, timeout, output_limit, Some(input))
}

fn run_inner(
    mut command: Command,
    timeout: Duration,
    output_limit: usize,
    input: Option<&[u8]>,
) -> io::Result<Output> {
    validate_arguments(&command)?;
    let token = current_cancellation().or_else(|| {
        crate::signals::current().map(|flag| CancellationToken::with_interrupt(Some(flag)))
    });
    if token.as_ref().is_some_and(CancellationToken::is_cancelled) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "helper cancelled",
        ));
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    wrapped
        .wrap(process_wrap::std::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ))
        .wrap(process_wrap::std::JobObject);
    let mut child = OwnedChild {
        child: wrapped.spawn()?,
        stopped: false,
    };
    let mut stdout = Pipe::new(child.child.stdout().take().expect("piped stdout"))?;
    let mut stderr = Pipe::new(child.child.stderr().take().expect("piped stderr"))?;
    #[cfg(unix)]
    let mut stdin = child.child.stdin().take();
    #[cfg(unix)]
    if let Some(pipe) = &stdin {
        pipe.prepare()?;
    }
    #[cfg(unix)]
    let mut remaining = input.unwrap_or_default();
    let start = Instant::now();
    let status = loop {
        if token.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "helper cancelled",
            ));
        }
        if start.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("command timed out after {}ms", timeout.as_millis()),
            ));
        }
        stdout.drain(output_limit)?;
        stderr.drain(output_limit)?;
        #[cfg(unix)]
        if let Some(pipe) = &mut stdin {
            use std::io::Write;
            if !remaining.is_empty() {
                match pipe.write(&remaining[..remaining.len().min(8192)]) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "helper stopped reading input",
                        ));
                    }
                    Ok(count) => remaining = &remaining[count..],
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
            if remaining.is_empty() {
                stdin.take();
            }
        }
        if let Some(status) = child.child.try_wait()? {
            break status;
        }
        std::thread::sleep(POLL);
    };
    // The launcher may have exited while a descendant still owns its pipes.
    // End that tree before draining remaining buffered output.
    child.stop();
    #[cfg(unix)]
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "helper exited before receiving its input",
        ));
    }
    let until = Instant::now() + CLEANUP;
    while !(stdout.eof && stderr.eof) && Instant::now() < until {
        stdout.drain(output_limit)?;
        stderr.drain(output_limit)?;
        if !(stdout.eof && stderr.eof) {
            std::thread::sleep(POLL);
        }
    }
    if !(stdout.eof && stderr.eof) {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "helper output pipes did not close after process-tree cleanup",
        ));
    }
    Ok(Output {
        status,
        stdout: stdout.output,
        stderr: stderr.output,
    })
}

pub enum StreamControl {
    Continue,
    Reply(Vec<u8>),
    Finish,
}

/// Run a bounded, prompt-free helper control dialogue without retaining output.
/// Replies use nonblocking stdin; all exit paths clean up the process tree.
#[cfg(unix)]
pub fn control_dialogue(
    mut command: Command,
    input: &[u8],
    timeout: Duration,
    output_limit: usize,
    mut observer: impl FnMut(&[u8]) -> io::Result<StreamControl>,
) -> io::Result<Option<ExitStatus>> {
    use std::io::Write;
    if input.len() > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "control request exceeded limit",
        ));
    }
    validate_arguments(&command)?;
    let token = current_cancellation().or_else(|| {
        crate::signals::current().map(|flag| CancellationToken::with_interrupt(Some(flag)))
    });
    if token.as_ref().is_some_and(CancellationToken::is_cancelled) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "helper cancelled",
        ));
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    wrapped.wrap(process_wrap::std::JobObject);
    let mut child = OwnedChild {
        child: wrapped.spawn()?,
        stopped: false,
    };
    let mut stdout = Pipe::new(child.child.stdout().take().expect("piped stdout"))?;
    let mut stderr = child.child.stderr().take().map(Pipe::new).transpose()?;
    let mut stdin = child.child.stdin().take();
    #[cfg(unix)]
    if let Some(pipe) = &stdin {
        pipe.prepare()?;
    }
    let mut pending = input.to_vec();
    let mut written = 0;
    let mut received = 0_usize;
    let start = Instant::now();
    let mut exited = None;
    let mut cleanup_deadline = None;
    let mut finished = false;
    let mut finish_deadline = None;
    loop {
        if token.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "helper cancelled",
            ));
        }
        if start.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "control dialogue timed out",
            ));
        }
        stdout.drain_with(&mut |bytes| {
            received = received.saturating_add(bytes.len());
            if received > output_limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "helper output exceeded limit",
                ));
            }
            if finished {
                return Ok(());
            }
            match observer(bytes)? {
                StreamControl::Continue => {}
                StreamControl::Finish => finished = true,
                StreamControl::Reply(reply) => {
                    if stdin.is_none() || written != pending.len() || reply.len() > 4096 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "invalid control reply",
                        ));
                    }
                    pending = reply;
                    written = 0;
                }
            }
            Ok(())
        })?;
        if finished && finish_deadline.is_none() {
            // EOF lets cooperative helpers flush their own caches and exit.
            // Uncooperative descendants still get the bounded tree cleanup.
            stdin.take();
            pending.clear();
            written = 0;
            finish_deadline = Some(Instant::now() + CLEANUP);
        }
        if finish_deadline.is_some_and(|until| Instant::now() >= until) {
            return Ok(None);
        }
        if let Some(stderr) = &mut stderr {
            stderr.drain_with(&mut |bytes| {
                received = received.saturating_add(bytes.len());
                if received > output_limit {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "helper output exceeded limit",
                    ));
                }
                Ok(())
            })?;
        }
        if written < pending.len() {
            match stdin
                .as_mut()
                .expect("dialogue stdin")
                .write(&pending[written..])
            {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "helper closed stdin",
                    ));
                }
                Ok(count) => written += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if exited.is_none()
            && let Some(status) = child.child.try_wait()?
        {
            exited = Some(status);
            child.stop();
            cleanup_deadline = Some(Instant::now() + CLEANUP);
        }
        if exited.is_some() && stdout.eof && stderr.as_ref().is_none_or(|pipe| pipe.eof) {
            return Ok(if finished { None } else { exited });
        }
        if cleanup_deadline.is_some_and(|until| Instant::now() >= until) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "helper output pipes did not close",
            ));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(not(unix))]
pub fn control_dialogue(
    _command: Command,
    _input: &[u8],
    _timeout: Duration,
    _output_limit: usize,
    _observer: impl FnMut(&[u8]) -> io::Result<StreamControl>,
) -> io::Result<Option<ExitStatus>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "control dialogues require nonblocking stdin",
    ))
}

struct OwnedChild {
    child: Box<dyn ChildWrapper>,
    stopped: bool,
}

impl OwnedChild {
    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        let _ = self.child.start_kill();
        // Poll rather than calling wait() on an entire Windows job indefinitely.
        let until = Instant::now() + CLEANUP;
        while Instant::now() < until {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(POLL),
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Pipe<R> {
    reader: R,
    output: Vec<u8>,
    eof: bool,
}

impl<R: Read + PipeHandle> Pipe<R> {
    fn new(reader: R) -> io::Result<Self> {
        reader.prepare()?;
        Ok(Self {
            reader,
            output: Vec::new(),
            eof: false,
        })
    }

    fn drain(&mut self, limit: usize) -> io::Result<()> {
        let mut output = std::mem::take(&mut self.output);
        let result = self.drain_with(&mut |bytes| {
            let keep = bytes.len().min(limit.saturating_sub(output.len()));
            output.extend_from_slice(&bytes[..keep]);
            Ok(())
        });
        self.output = output;
        result
    }

    fn drain_with(&mut self, receive: &mut impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
        let mut buffer = [0; 8192];
        // A continuously writing process must not starve timeout/cancellation.
        for _ in 0..32 {
            if self.eof {
                break;
            }
            let available = match self.reader.available(buffer.len()) {
                Ok(0) => break,
                Ok(available) => available,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                    self.eof = true;
                    break;
                }
                Err(error) => return Err(error),
            };
            match self.reader.read(&mut buffer[..available]) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(count) => {
                    receive(&buffer[..count])?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

trait PipeHandle {
    fn prepare(&self) -> io::Result<()>;
    fn available(&self, capacity: usize) -> io::Result<usize>;
}

#[cfg(unix)]
impl<T: std::os::fd::AsRawFd> PipeHandle for T {
    fn prepare(&self) -> io::Result<()> {
        // SAFETY: this is an owned pipe descriptor, still alive for both calls.
        let flags = unsafe { libc::fcntl(self.as_raw_fd(), libc::F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(self.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn available(&self, capacity: usize) -> io::Result<usize> {
        Ok(capacity)
    }
}

#[cfg(windows)]
impl<T: std::os::windows::io::AsRawHandle> PipeHandle for T {
    fn prepare(&self) -> io::Result<()> {
        Ok(())
    }
    fn available(&self, capacity: usize) -> io::Result<usize> {
        use windows::Win32::{Foundation::HANDLE, System::Pipes::PeekNamedPipe};
        let mut available = 0;
        // SAFETY: the live owned pipe handle and writable count are valid.
        // Only this runner reads the pipe, so reading this many bytes cannot block.
        unsafe {
            PeekNamedPipe(
                HANDLE(self.as_raw_handle()),
                None,
                0,
                None,
                Some(&mut available),
                None,
            )
        }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))?;
        Ok(capacity.min(available as usize))
    }
}

/// Truncation may split a UTF-8 code point; omit only an incomplete final one.
pub fn decode_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(error) if error.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}
