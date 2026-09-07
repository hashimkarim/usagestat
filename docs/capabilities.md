# Capabilities and read-only diagnostics

`usagestat capabilities --json` and `GET /v1/capabilities` expose the additive
schema version 1 contract. Existing `/health`, provider arrays and usage metric
fields retain their shape. Clients should ignore unknown fields and feature keys,
and treat a missing capability endpoint on an older backend as unknown support.

The report includes `backendVersion`, `apiVersion`, `os`, `architecture`,
`profile`, `serviceManager`, `features`, `helpers`, `providers`, and
`diagnosticStates`. OS values follow Rust (`linux`, `macos`, `windows`), and
architectures use `x86_64`/`aarch64`. The service manager is `systemd`, `launchd`,
`task-scheduler`, or `none`.

Each feature separates three questions:

| Field | Meaning |
| --- | --- |
| `implemented` | This build contains the platform implementation. |
| `runtime` | `available`, `unavailable`, `unsupported`, or `not-checked`. |
| `qualification` | Evidence scope, such as native fixtures, session tests pending, or unverified. |
| `reasonCode` | Optional stable reason for unavailability. |

The feature keys cover daemon foreground/autostart/authenticated shutdown and
`daemon.unregister` for managed login-registration removal,
`daemon.independentControls` for separate running/login-startup controls,
credential operations, automatic browser import/manual credentials, helper
process cleanup, and structured provider states. Credential availability remains
`not-checked`: constructing capabilities never opens an OS credential store.
Helpers are only resolved on PATH (`found`/`not-found`); discovery does not prove
that a helper can run or is authenticated. Provider `declaredSources` and
`autoSource` describe its manifest. `qualification: unverified` and
`authentication: not-checked` are deliberate until provider-specific native
qualification exists. The HTTP inventory reflects providers loaded by the daemon.

Run `usagestat doctor --json` for an installed diagnostic report, or omit `--json`
for text with repair guidance. Doctor uses the same native path resolver as the
backend and reports checks with `id`, `code`, and `action`. It resolves local
config/data paths, validates configuration, discovers resources and the backend,
queries the native service manager, and requests only the local `/health`
endpoint. It does not create files, repair settings, register/start services,
read management keys, probe providers, or access credential stores. `--config`
and `--plugin-dir` are supported. Service status uses the saved daemon profile.

Stable diagnostic codes include `config-missing`, `config-invalid`,
`directory-missing`, `path-unavailable`, `binary-missing`, `resources-missing`,
`service-stopped`, `service-manager-unavailable`, `service-settings-unavailable`,
`installation-owner-mismatch`, `wrong-version`, `unhealthy`, and `no-data`.
Doctor's `service.condition` preserves the more detailed daemon status condition,
including `port-conflict`. Its cached provider state counts describe previous
probes; old snapshots without state are `unknown`. Neither an empty cache nor an
unchecked store means the user lacks authentication. JSON collection succeeds
even when checks report problems; clients should inspect the codes.

Usage snapshots optionally add `state`, using these stable values:

| State | Meaning |
| --- | --- |
| `ready` | The probe returned metrics. |
| `unsupported` | The requested method/source is unavailable. |
| `missing-auth` | A probe explicitly reported absent authentication. |
| `no-data` | The probe returned no metrics or explicitly reported no data. |
| `credential-denied` | The OS refused credential access. |
| `credential-unavailable` | The credential store could not be accessed, including a locked store. |
| `credential-account-mismatch` | An exact target belongs to a different account. |
| `credential-malformed` | Stored credential encoding/format is invalid. |
| `timed-out` | A probe exceeded its deadline. |
| `failed` | An otherwise unclassified failure. |

Plugins may return a `state` with their metrics or throw an object with an error
`code` from this vocabulary and a safe `message`. Existing error badges remain
available to old consumers. Host credential errors and explicit legacy login
and timeout messages map to states; arbitrary errors remain `failed`.

Automatic browser import has native fixture coverage on Linux, macOS and Windows.
Windows App-Bound (`v20`) cookies remain explicitly unsupported with manual
credential guidance. Real desktop/browser account qualification is tracked by
#18/#19; fixture coverage does not claim real-account provider support.

The native gate runs `tools/portability/diagnostics.py`: it checks installed
missing files/helpers, invalid settings without value disclosure, cached
denied/locked states, occupied ports, version mismatches, synthetic plugin errors,
and unchanged filesystem contents/timestamps across doctor runs. The daemon
lifecycle fixture also verifies the additive endpoint alongside existing arrays.

The [native gate at `713bfec`](https://github.com/hashimkarim/usagestat/actions/runs/34077905565)
passed on September 7, 2026 on Windows x64, macOS Intel/Apple Silicon and Linux
x64/ARM64, including the installed diagnostic checks on every target. The bar's
consumer integration is tracked separately in #17.
