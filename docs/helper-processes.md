# Native helper execution

Tracking issue: [#5](https://github.com/Hashim-K/usagestat/issues/5).
Implementation: [`usagestat-core::process`](../crates/usagestat-core/src/process.rs).

## Discovery and arguments

`USAGESTAT_HELPER_PATH` is an optional complete helper search path, using the OS
path separator (`:` on Unix and `;` on Windows). A nonempty value takes precedence
over inherited `PATH` and built-in helper locations. Set it in the background
service's environment to include the required tool and runtime directories.
An empty value uses normal discovery. Shell startup files are never executed.

Without an override, ccusage retains its Linux/macOS Homebrew, Bun, and nvm
locations and its existing runner order: bunx, pnpm, yarn, npm, then npx. Windows
adds native roaming npm, `PNPM_HOME`, `NVM_SYMLINK`, and the user's Bun directory
to inherited `PATH`. Other helpers use inherited `PATH`; firectl also retains its
Homebrew fallbacks. Resolution produces an absolute executable path and does not
implicitly search the current directory. A relative entry explicitly present in
`PATH` retains its normal meaning.

Native executables receive separate arguments. On Windows, recognized npm, npx,
pnpm, and yarn batch shims with a known adjacent JavaScript entry point run via
Node directly, preserving spaces, Unicode, quotes, and shell metacharacters.
Other `.cmd`/`.bat` helpers use Rust's batch-file argument handling with a
conservative restriction: control characters and `" % & | < > ^ !` in their path
or arguments produce an actionable error. Arbitrary arguments require a native
executable or a supported Node entry point. No provider arguments are assembled
into a `cmd /c` string. See Rust's
[Windows argument documentation](https://doc.rust-lang.org/std/process/index.html#windows-argument-splitting).

The plugin host still permits only `gh` through `host.command.run`, with the
existing 30-second maximum and output caps. Executable lookup does not broaden
that allowlist. Dedicated ccusage and firectl host APIs retain their own limits.

## Deadlines and cleanup

The shared runner uses Unix process groups and Windows Job Objects through
[`process-wrap`](https://docs.rs/process-wrap/10.0.0/process_wrap/). Windows
helpers are assigned to their job before they resume and use `CREATE_NO_WINDOW`.
Each invocation closes stdin, drains stdout and stderr without blocking reader
threads, and retains at most the requested number of bytes per stream. Excess
output is drained so a full pipe cannot deadlock the helper.

Cancellation, timeout, errors, and normal parent exit trigger cleanup of the
owned helper tree. Polling and pipe draining have bounded cleanup intervals,
including when a descendant inherited the output handles. This covers ordinary
descendants; deliberately detached Unix processes and abrupt termination of
usagestat itself require separate lifecycle consideration.

CLI probe deadlines and interrupt flags propagate into synchronous host calls
and the QuickJS interrupt handler. The CLI gives the worker up to two seconds to
clean up before returning a timeout or exiting with status 130 on an interrupt.
The daemon connects the same shutdown flag to its provider worker and waits up
to three seconds for cleanup. Blocking HTTP calls retain their transport
deadlines; cancellation does not abort an in-flight HTTP request.

The shared runner covers ccusage, gh, firectl, read-only process discovery,
macOS `security` operations, and Linux `secret-tool` operations. Secret Service
writes use a nonblocking stdin pipe, sharing the same deadline, cancellation, and
output-draining loop. No secret is moved into a temporary input file.

## Validation and remaining qualification

`cargo test --locked -p usagestat-core --test process` compiles a small native
helper fixture and checks arguments, output caps, closed stdin, minimal search
paths, cancellation, timeout, and descendant cleanup after parent exit. Windows
adds Node-shim and restricted batch-shim cases.

`python tools/portability/probe_cancellation.py --binary target/debug/usagestat`
checks the real CLI and daemon with an isolated synthetic plugin: helper timeout,
a spinning JavaScript probe, host command rejection, Unix SIGINT/SIGTERM, and
Windows Ctrl+C, Ctrl+Break, and closure of an owned pseudoconsole. The native
gate runs both suites alongside installed-binary smoke checks. It creates no
provider accounts and reads no real provider credentials.

All five initial targets passed the complete native gate at
[`5daf9bc`](https://github.com/hashimkarim/usagestat/commit/5daf9bc), in
[run 34071262106](https://github.com/hashimkarim/usagestat/actions/runs/34071262106).
Windows executes nine native process fixtures and nine CLI/daemon probe/shutdown
checks, including descendant termination after actual terminal closure. The
closure test owns its ConPTY and drains its output; it does not send events to
the runner's own console. A window-message-based prototype did not close the
hosted console and was replaced with the native terminal lifecycle API.

User-session logoff/reboot, service-manager stop behavior, and real provider
integration remain in the service and final qualification issues. Passing native
console tests does not establish those separate lifecycle behaviors.
