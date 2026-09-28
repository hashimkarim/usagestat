# T3 Code usage limits

`usagestatd` can act as a read-only CLIProxyAPI usage hub for T3 Code. It exposes
the cached Claude and Codex subscription quotas through both current and legacy
hub contracts:

```text
GET /v0/management/quota-scheduler/status
GET /v0/management/auth-files
POST /v0/management/api-call
Authorization: Bearer <management-key>
```

`auth-files` lists synthetic account IDs, never credential contents. `api-call`
accepts only `GET` usage requests for the exact Claude and Codex quota URLs and
returns normalized cached snapshots. It does not proxy requests or substitute
tokens. Reset-credit redemption, credential changes, arbitrary URLs, and provider
writes are rejected. Current T3 releases use these two endpoints instead of the
legacy status endpoint.

## Enable automatic startup

Build the daemon from this repository:

```bash
cargo build --release -p usagestat-cli -p usagestat-daemon
./target/release/usagestat daemon t3 auto
./target/release/usagestat daemon enable
```

On Linux with systemd, these commands select T3's `auto` mode, start the daemon
now, enable it at login, and generate a management key at
`~/.config/usagestat/t3-management-key`. Repeating the command keeps the same key
and applies updated service settings. No root privileges are required. Autostart
is off until you enable it. Plain `daemon enable` remembers the last T3 mode;
on first use, it serves the dashboard and native all-provider API with T3 off.

Paths honor `XDG_CONFIG_HOME` and `XDG_DATA_HOME`. The command and `daemon status`
print the actual management-key location, which may differ inside an app's
terminal. The service also works when the app and systemd use different config
directories.

Retrieve the key to paste into T3:

```bash
./target/release/usagestat daemon key
```

After installing the CLI and daemon together, use `usagestat daemon t3 auto`,
`usagestat daemon enable`,
`usagestat daemon status`, `usagestat daemon key`, and `usagestat daemon disable`
directly. Plain `disable` stops the daemon and turns off startup at login,
retaining the T3 mode and key for later use. The dev installers provide the same
commands through `usagestat-dev`, with a separate service, configuration, and cache.

To control only the T3 bridge:

```bash
usagestat daemon t3 auto  # Expose the bridge whenever the daemon runs
usagestat daemon t3 off   # Keep the bridge disabled
```

These commands preserve your bind address, config, plugin paths, and autostart
setting. A running daemon restarts to apply a change and continues serving the
dashboard and native API. A stopped daemon stays stopped. The key is retained
when T3 is turned off. The next `daemon enable` uses the saved preference from
`~/.config/usagestat/daemon.json`; you can also set it before first starting the
service.

Auto follows the daemon's lifecycle; it does not detect T3 or start the daemon.
T3 connections require a running daemon. `daemon status` reports both the saved
mode and current local availability, for example
`T3: auto · unavailable (daemon stopped)`. An available bridge has answered an
authenticated quota request. JSON status includes `t3Mode` and `t3Available`.
The existing `enable --t3`, `disable --t3`, and `toggle --t3` shortcuts still work.

Custom config, plugins, binary location, and bind address can be supplied when
enabling the service:

```bash
usagestat --config /path/to/config.toml --plugin-dir /path/to/plugins \
  daemon enable --binary /path/to/usagestatd --bind 127.0.0.1:6736
```

The service preserves the invoking command's plugin paths and `PATH`. Put any
additional provider environment variables in `~/.config/usagestat/daemon.env`
(`NAME=value` lines, mode 600), then repeat `daemon enable` to restart it with the
saved T3 mode.
Provider credentials already stored in the backend config continue to work.
Managed services ignore `USAGESTAT_MANAGEMENT_KEY` from the environment so that
T3 compatibility is controlled by the saved mode and key file.

## Run manually

You can still start the daemon in the foreground, using the generated key:

```bash
./target/release/usagestatd \
  --management-key-file ~/.config/usagestat/t3-management-key
```

The usual `--config`, `--plugin-dir`, `--bind`, and `--refresh-sec` options also
apply. Run from the repository root to discover its bundled plugins, or pass
their location with `--plugin-dir`. Make sure `claude` and/or `codex` are enabled
and can fetch quota data with your existing usagestat credentials. If you already
run a daemon, restart it using the updated binary and key option.

Alternatively, set `USAGESTAT_MANAGEMENT_KEY` in the daemon's environment. A key
file takes precedence. Empty or malformed keys fail startup; with neither option
set, the management endpoint is disabled and returns HTTP 404. The key is separate
from your provider credentials. `X-Management-Key` is also accepted for
CLIProxyAPI clients.

For manual-only use, generate a key once with
`(umask 077; openssl rand -hex 32 > ~/.config/usagestat/t3-management-key)` after
creating the config directory, or set `USAGESTAT_MANAGEMENT_KEY` yourself.

## Connect T3 Code

In **Settings → Usage providers → Add hub**, enter:

| Field | Value |
| --- | --- |
| Hub URL | `http://127.0.0.1:6736` |
| Management key | Contents of `~/.config/usagestat/t3-management-key` |
| Label | `usagestat` (or any label you prefer) |

The URL must be reachable **from the T3 server** that owns the usage-provider
settings. For the same machine, the default loopback address works. For a remote
T3 server, bind the daemon to a reachable private address and use that address in
T3; use an HTTPS reverse proxy if the connection crosses an untrusted network.
The management key protects this compatibility endpoint; the existing `/v1/*`
API retains its local, unauthenticated behavior.

Open **Usage → Limits**. T3 polls the hub when its settings change and on its
provider health refresh interval. usagestat refreshes its cached quotas on its
own polling interval (60 seconds by default); a hub read returns the current
cache immediately. Current-protocol reads reject failed snapshots and readings
older than ten minutes, because that protocol cannot display the original fetch
timestamp. It must not present a stale cached reading as a new measurement.

## Supported data

| usagestat metric | CLIProxyAPI window | T3 display |
| --- | --- | --- |
| Claude `Session` | `five_hour` | Session |
| Claude `Weekly` | `seven_day` | Weekly |
| Claude `Fable`, when available | `fable` | Weekly · Fable |
| Codex `Session` | `five_hour` | Session |
| Codex `Weekly` | `weekly` | Weekly |

Percentages use `used / limit`, bounded to 0–100. Reset and fetch timestamps come
from the snapshot. Missing, failed, or invalid quotas are omitted, so unavailable
usage does not become a misleading 0%. Disabled or removed providers are excluded.
Codex plan labels are converted to the slugs T3 expects, including Pro 5x and
Pro 20x.

The current daemon stores one snapshot per provider, so this adapter exposes
`claude.json` and `codex.json` as stable account IDs. These are synthetic IDs, not
credential files. It does not enumerate multiple logins or expose authentication
files. T3 currently renders only Claude and Codex hub accounts, and ignores other
provider types and quota windows. Sonnet/Opus-specific limits, Codex review/model
limits, spending, and token history remain available through the native usage
API rather than this T3 view. T3 labels Claude accounts “Claude Subscription”
regardless of their tier.

This restriction is in current T3 Code's hub reader. Sending additional provider
names or metric fields does not make T3 render them. A backend-only integration
cannot show every provider on T3's Usage page. The full provider information is
available in the backend's [dashboard](http://127.0.0.1:6736/dashboard) and native
`GET /v1/providers` and `GET /v1/usage` endpoints; showing it
natively in T3 would require T3 to add support for those fields and providers.

The adapter follows T3's
[usage source fetcher](https://github.com/pingdotgg/t3code/blob/main/apps/server/src/usage/UsageLimitSources.ts)
and current `cliproxyApi.ts` request shapes, verified against the installed T3
Nightly runtime on September 27, 2026. It implements cached usage compatibility,
not CLIProxyAPI inference, credential management, or network proxying.
