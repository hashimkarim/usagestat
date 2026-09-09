# usagestat native backend through npm

Install the v2 backend alpha explicitly through the `alpha` npm tag. It contains
the CLI, daemon, local dashboard and provider plugins. Windows/macOS bar frontends
are not included. Desktop login/reboot, browser authentication and minimum-OS
acceptance remain pending; Windows/macOS binaries are unsigned.

```sh
npm install --global @hashimkarim/usagestat@alpha --include=optional --ignore-scripts
usagestat --version
usagestat doctor
usagestat --json list
```

Use Node.js 24 or newer and npm 11.5.1 or newer. Exact-version optional platform
packages contain the Rust CLI, daemon, plugins, icons and license notices.
Installation needs no Rust compiler, shell scripts or download hooks. It does
not start a daemon or install a login service. Native packages also remain
available separately for users who do not need Node.

Candidate npm targets are Linux x64/ARM64 with glibc 2.39+, macOS 13.5+ on Intel/Apple
Silicon, and Windows 10 / Server 2016+ x64. musl/Alpine, Windows ARM64 and 32-bit
systems are unsupported. Stable packages include only qualified targets;
macOS/Windows minimum-system qualification and signing are still pending.
The Rust macOS artifact targets 11.0; the npm wrapper's higher floor comes from
[Node.js 24's platform requirements](https://github.com/nodejs/node/blob/v24.x/BUILDING.md#platform-list).

To use a background daemon after a durable global installation:

```sh
usagestat daemon enable
usagestat daemon status
usagestat dashboard
```

The native CLI registers its actual sibling backend with the current user's
systemd, LaunchAgent or Task Scheduler. Login startup does not require Node or
an interactive shell. Configuration, history, T3 keys and credentials stay in
the usual per-user locations outside npm's package/cache directories. Existing
native/Homebrew/bar installations retain ownership until an explicit owner
switch. Use `daemon enable --switch-owner` only when intentionally transferring
this profile to the npm installation.

For an explicit update, record the old package version and `daemon status --json`,
including its independent `running` and `autostart` values. For an existing managed
installation, stop and disable the daemon before replacing its files. This releases
Windows executable locks. Use the same durable npm global prefix:

```sh
usagestat daemon status --json
usagestat daemon disable
npm install --global @hashimkarim/usagestat@VERSION --include=optional --ignore-scripts
usagestat --version
usagestat doctor
```

After a successful install, restore the saved state using the retained registration:

| Previous running / autostart | Commands after installation |
| --- | --- |
| Yes / Yes | `usagestat daemon autostart on`, then `usagestat daemon start` |
| No / Yes | `usagestat daemon autostart on`; leave it stopped |
| Yes / No | `usagestat daemon start`; leave autostart off |
| No / No | Leave both off |

An installation that never registered a daemon needs only the npm install and
verification steps. T3 intent, keys and user data survive replacement. Verify
health with `daemon status --json` after starting. If installation is interrupted
or the replacement fails, keep startup off and reinstall the recorded previous
exact npm version, then restore the state above. Keep that package available until
the update is verified. These are explicit recovery steps; npm replacement is not
an automatic transaction. When changing a Node version manager or npm global
prefix, unregister the old installation first, install into the new durable
prefix, and explicitly transfer ownership with `daemon enable --switch-owner`.
Restore the previous running/autostart preferences afterward.

Before removal:

```sh
usagestat daemon unregister
npm uninstall --global @hashimkarim/usagestat --ignore-scripts
```

Unregister removes the owned login entry while retaining user data and saved
preferences. Lifecycle hooks are never required for cleanup. Do not
remove the files of a running Windows service. A one-off
`npm exec --package=@hashimkarim/usagestat@alpha -- usagestat --version` is suitable for
CLI use; persistent startup from the temporary `_npx`/`_cacache` path is rejected.
Project-local installations can own startup only while their directory remains
durable. Prefer a global installation for services.

`native-package-missing` means optional dependencies were omitted: reinstall with
`--include=optional`. Version/integrity failures require reinstalling the exact
main-package version. A package cannot remove native OS/glibc requirements.
On Windows, use npm's generated `.cmd` command shims from Command Prompt, or the
`.cmd` command from PowerShell when its script policy blocks the generated `.ps1`.
