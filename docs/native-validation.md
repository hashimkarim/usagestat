# Native foundation validation

Latest complete native evidence: [`354c390`](https://github.com/hashimkarim/usagestat/commit/354c390)
passed [run 34096607532](https://github.com/hashimkarim/usagestat/actions/runs/34096607532)
on all five targets, including the aggregate evidence gate and all ten Windows
installer scenarios. Downloaded reports were independently verified against that
exact clean commit. This includes the retained-daemon content check for relocation
recovery. The isolated real Linux systemd lifecycle/relocation test also passed
locally using a uniquely named unit; the existing installed daemon was preserved.

The same commit passed the full
[release rehearsal 34096637011](https://github.com/hashimkarim/usagestat/actions/runs/34096637011):
five native archives, five npm installations, both nine-check Homebrew suites and
all ten Windows installer scenarios using the verified release ZIP. The downloaded
candidate's 14 checksum sidecars and five manifest source identities were also
checked locally. Publication jobs were skipped. These unsigned candidates remain
subject to the [desktop qualification handoff](desktop-qualification.md).

The implementation branch passed the complete native foundation gate at
[`a01c9fc`](https://github.com/hashimkarim/usagestat/commit/a01c9fc) in
[run 34069426577](https://github.com/hashimkarim/usagestat/actions/runs/34069426577).
Both binaries build and execute on Linux x64/ARM64, macOS Intel/Apple Silicon,
and Windows x64 MSVC. This completes the build/runtime CI foundation in #3;
native services, credentials, distribution, and minimum OS versions have their
own remaining qualification requirements.

| Target | Native runner | Gate result |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | Ubuntu 24.04 x64 | Passed |
| `aarch64-unknown-linux-gnu` | Ubuntu 24.04 ARM64 | Passed |
| `aarch64-apple-darwin` | macOS 15 ARM64 | Passed |
| `x86_64-apple-darwin` | macOS 15 Intel | Passed |
| `x86_64-pc-windows-msvc` | Windows Server 2025 x64 | Passed; repeated with restored cache |

Each job uploads `report.json` and command logs containing the executing Rust
host, OS, runner image, Python/Node versions, native dependency versions, exact
test commands, exit codes, and runtime results. Native execution is required by
the gate; cross-compilation is not counted as runtime evidence.

The suite includes Rust tests, dashboard JavaScript tests, provider inventory,
both installed executables outside the checkout, absolute icon discovery,
QuickJS, bundled SQLite, local host HTTP/filesystem operations, isolated writable
state, daemon polling/health/JSON, helper timeouts, and spinning-probe cancellation.
Unix jobs also check SIGINT/SIGTERM. Windows executes native argument, npm shim,
batch shim, cancellation, and descendant cleanup fixtures. Console event coverage
is tracked separately in #5.

The first run, [34069032354](https://github.com/hashimkarim/usagestat/actions/runs/34069032354),
passed every Rust test on Windows but stopped when Python printed a Unicode Node
test glyph using its legacy console encoding. The harness now uses UTF-8 and
records each command's outcome before printing its log. Linux and macOS passed
that initial run and the subsequent run. Windows was rerun after its first
successful run to exercise the restored native build cache.

These fixtures use no provider credentials. The local HTTP fixture initializes
the production reqwest/rustls client but does not prove an external TLS handshake,
corporate proxy behavior, browser import, real provider authentication, or a
minimum OS floor. Record those separately during their owning qualification
issues instead of inferring support from this gate.

## Portable paths and installed resources (#4)

At [`3969111`](https://github.com/hashimkarim/usagestat/commit/3969111),
[run 34070642719](https://github.com/hashimkarim/usagestat/actions/runs/34070642719)
passed the path unit tests and all 16 installed-runtime checks on every target
above. Windows completed these checks and all Rust tests before a later console
shutdown test failed; that separate failure remains tracked in #5.

The installed checks discover every committed provider manifest and absolute icon
path from an unrelated working directory. They exercise flat archives, prefix
share/lib layouts, npm bin/resources, macOS app resources, and dev binaries. They
also exercise Unicode/spaces, explicit redirected config/data directories, absent
HOME on Windows, and a read-only installation tree. Pure resolver tests verify
missing-native-directory errors and Windows `.exe` profile identity. Windows
defaults use the Known Folder APIs through `dirs`; changing APPDATA alone is not
claimed as a real Windows Known Folder redirection test.

CLI, daemon, plugin host and doctor use the same fallible core path API.
The [capabilities and diagnostics contract](capabilities.md) describes the
read-only doctor command. Actual provider data/credential paths remain #11.

## Private state and helper lifecycle (#5, #6)

The full gate passed on every target at
[`5daf9bc`](https://github.com/hashimkarim/usagestat/commit/5daf9bc), in
[run 34071262106](https://github.com/hashimkarim/usagestat/actions/runs/34071262106).
This adds private native ACL/mode checks, locked-file and failed-write recovery,
concurrent initialization, forced writer termination, and CLI/daemon shutdown.
Windows verifies Ctrl+C, Ctrl+Break, and actual ConPTY closure; Unix verifies
SIGINT/SIGTERM. See [private state](private-state.md) and
[helper execution](helper-processes.md) for guarantees and remaining service/
provider/session qualification boundaries.

## Complete evidence gate

The native workflow now verifies the downloaded reports after all five jobs.
`tools/portability/verify_native_reports.py` requires the exact clean source commit,
every native target, all fixture sections and successful test commands. It also
requires the actual named LaunchAgent/scheduled-task test result in its log and
the Windows install/rollback/recovery/uninstall report. A successful command that
ran zero selected native tests does not qualify the service adapter.

Any recorded exception is a failure, including an empty assertion message. This
corrects a reporting defect discovered while developing the Windows installer:
runs 34089031317 and 34090014295 stopped in dev installation on macOS/Windows, but
reported green because `str(AssertionError())` is empty. Their uploaded reports
retain that failure. Completed earlier sections can be assessed individually;
the omitted dev/service phases cannot be counted as passed. Path comparisons now
handle Windows extended/short names and macOS `/var` aliases by file identity.

This aggregate gate establishes fixture completeness. Real desktop login/reboot,
minimum OS, consenting provider accounts, signing/Gatekeeper and actual native bar
acceptance remain separate requirements in #20.

The complete gate passed at `3ae3be1` in
[run 34094685300](https://github.com/hashimkarim/usagestat/actions/runs/34094685300).
All five reports were downloaded and rechecked against the exact source commit;
the Windows credential stress and named scheduled-task test, both named Mac
LaunchAgent tests, Python checks and seven Windows installer checks completed.

The same source passed
[release rehearsal 34094784327](https://github.com/hashimkarim/usagestat/actions/runs/34094784327):
five release archives and npm installations, both nine-check Homebrew rehearsals
with active relocation/recovery, and the seven-check Windows installer using the
verified release ZIP and downloaded script. Publication jobs were skipped.
The [desktop qualification handoff](desktop-qualification.md) records the
remaining acceptance matrix and the evidence needed to close the open issues.
