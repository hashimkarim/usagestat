# Private backend state

Tracking issue: [#6](https://github.com/hashimkarim/usagestat/issues/6).
Implementation: [`usagestat-core::storage`](../crates/usagestat-core/src/storage.rs).

All backend-produced state files use shared private storage: management keys,
daemon settings and environment/unit files, snapshot cache, daily usage,
history appends, provider `host.fs.writeText` updates, and temporary Fireworks
credentials. The backend currently reads user-supplied config files; it does not
rewrite them merely to validate or load configuration.

Files are restricted before credential bytes are written. Unix files use mode
0600 and newly created directories use 0700. Existing ancestor directories are
left intact; application-owned leaf directories are restricted explicitly.
Windows files and directories use a protected DACL granting full access only to
the current user and SYSTEM. Directory rules inherit to new children, and parent
ACLs cannot broaden an explicitly protected child. This uses native token and
file-security APIs, without invoking PowerShell or icacls in the application.
See Microsoft's [file-security contract](https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights).

Updates write to a randomly named temporary file in the destination directory,
sync its contents, and atomically replace the destination through
[`tempfile::NamedTempFile`](https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html).
There is no delete-before-replace fallback. Windows sharing/access errors are
retried for at most 500 ms, then reported while retaining the last valid state.
Failed writes clean up the unpublished temporary file. Atomic replacement avoids
partial JSON/token writes; it does not merge simultaneous updates or promise
power-loss durability for the directory entry.

Management keys retain their existing encoding and value. Creation publishes a
fully written key only if absent, so concurrent initializers read the same winner.
Existing empty or malformed keys produce an error and are never silently rotated.
Private reads reject symlinks/reparse points and tighten the file's permissions.
Explicit private-file destinations must be regular files; use native path
overrides to select another location instead of a symlink to a credential file.
Unix retains the existing config-directory management key. Windows uses the
local data directory for management/control keys, keeping them out of roaming
settings. The shared path helpers honor explicit application-directory overrides.

Fireworks exports use an unpredictable private temporary directory with scope
cleanup on success, error, and unwinding. Linux Secret Service writes use a
nonblocking stdin pipe with cancellation and output draining; credentials do not
enter command arguments or a temporary input file. Export failures log an exit
status instead of credential-bearing helper output. Forced process termination
can leave private temporary files; ordinary timeout and interrupt cleanup remains
part of the process/lifecycle checks.

The native test suite verifies access rules, complete concurrent initialization,
replacement, injected write failure, cleanup, Unix symlink rejection, and Windows
sharing contention. A separate process is terminated after writing unpublished
bytes; the previous committed file remains valid, and the next update succeeds.
These storage and key tests passed on all five initial targets at
[`cf388fb`](https://github.com/hashimkarim/usagestat/commit/cf388fb), in
[run 34070898869](https://github.com/hashimkarim/usagestat/actions/runs/34070898869).
That run's separate Windows window-close test failed after all storage and
installed-runtime checks passed; it is tracked in #5.
Credential-store APIs and browser/IDE authentication remain
separate issues; private files alone do not establish provider compatibility.
