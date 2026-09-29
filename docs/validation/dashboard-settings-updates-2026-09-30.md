# Dashboard settings and service update checks, 2026-09-30

These checks qualify the source changes after v2.0.0. They do not change or
republish the existing v2.0.0 tag, archives or package metadata.

## Automated checks

- `cargo test --locked`: 162 passed; one systemd-session test ignored.
- CLI and daemon builds with `--locked` passed.
- Node provider/dashboard tests: 355 passed, including 12 settings tests and
  seven Antigravity access regressions.
- Service synchronization and RPM Python tests: 22 passed, covering atomic executable
  replacement, stopped services, wrong ownership, SDK/unmanaged services,
  configuration races, failed replacement health, registered-settings discovery,
  package-owner filtering and post-transaction dispatch to existing user managers.
- Provider inventory and shared icon checks passed. Inventory hashes and
  fixture references were regenerated for the two changed Antigravity plugins.
- RPM recipes parse with the system's real systemd macros. Stable packages ship
  the stable feed and daily timer; alpha packages omit them. No standalone
  system installer remains.
- Private RPM packaging fixtures built for both channels using the real
  install/files/scriptlets. Payload and transaction metadata passed, including
  `%config(noreplace)` on the stable feed. These use placeholder executables and
  qualify packaging only; they are not release binaries and were not installed.
- All three packaged systemd units passed `systemd-analyze verify`.
- Publishing tests: 58 run, with two Windows-only checks skipped on Linux.

## Native dashboard preview

The rebuilt native daemon served an isolated local fixture profile with three
providers. `/dashboard/settings.js` matched the source asset byte for byte and
used `text/javascript`. Browser interaction verified:

- Manual provider hiding removes the card and tab and survives reloads.
- Display names, ordering and summary-quota choices persist.
- Light/dark/system appearance, accent and compact layout apply.
- Reset and undo restore preferences. Versioned import/export validates and
  strips credential fields; blocked storage remains a session-only preference.
- The Settings page fits a 390-pixel viewport without horizontal overflow.
- Overview totals follow manual visibility while saved History is retained.
- UsageStat Bar's 37 stored settings remained unchanged across dashboard edits.

The preview uses synthetic data and no credentials. It is UI qualification,
not provider/account compatibility or SDK ingestion evidence.

## Antigravity diagnosis

On the existing local account, normal OAuth refresh succeeded. Read-only Google
requests returned HTTP 200 for `loadCodeAssist`, HTTP 403 with a license/access
denial for quota endpoints, and HTTP 403 for available models. The installed
v2.0.0 plugin reports the old generic launch/login message. The rebuilt CLI with
the changed source plugin reports the quota access denial instead.

No prompt, execution, embedding, account migration or new credential scope was
created. The correction retains model-quota fallback after an optional denial
and still permits a normal refresh after HTTP 401. Missing usage stays unknown.

## Live installation boundaries

The normal RPM-owned readback daemon remains v2.0.0 at port 6736. The new user
`usagestat-service-sync.timer` is enabled and checks every two minutes. Its
initial live run reported `unchanged/current-executable`, preserving the
already-running service and its settings, data and T3 keys.

Following the user's packaging correction, the RPM now owns the upgrade hook,
stable feed configuration and optional daily download timer. The temporary
user watcher remains active until a subsequent RPM provides the hook. It is
not a required package installation step. The earlier request to run a
source-checkout root installer is superseded.

The new RPM payload is not installed on this host, which requires an interactive
sudo password. Before this work only the old alpha feed was configured. A
subsequent published package is needed for the settings page, corrected plugin
and packaged upgrade support to reach this RPM-owned installation. The daily
timer selects stable Usagestat and leaves dependency resolution to DNF; its
activation is a separate administrator choice.

The separate SDK-owned ingestion daemon at port 7436 was read only by this task.
Its owner subsequently upgraded it to the public v2.0.0 daemon, preserving its
configuration, credentials, bindings and 13 records. Read-only health and the
executable digest independently confirm the stable binary. This service is
outside the native readback update timer and was not provisioned from
release-validation credentials. Its owner has a separate private handoff.

The SDK owner subsequently confirmed public RC.2 publication and scoped rollout
completion: package source `91dc52292c627a6febed102a8198c56f35d3afc9`, final
receipts `27f2bb1aa5170a35ca5592d6db1eec2b73971bc9`. Their
[release evidence](https://github.com/agenticdriver/agenticdriver/blob/27f2bb1aa5170a35ca5592d6db1eec2b73971bc9/docs/validation/release-0.2.0-rc.2.md)
qualifies SDK compatibility with stable Usagestat v2.0.0. Dashboard changes here
retain their own validation and require a subsequent backend release.
