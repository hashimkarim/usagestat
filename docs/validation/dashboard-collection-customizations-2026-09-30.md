# Dashboard collection and customization checks, 2026-09-30

This qualifies source changes following `cd1467801dd834e1a09948b67f150b29afac44af`.
It does not publish a new release, alter the immutable v2.0.0 tag or replace an
installed daemon. The shared icon pin remains v0.1.0-alpha.1.

## Automated evidence

- Rust workspace: 162 passed; one systemd-session test ignored.
- CLI and daemon builds with `--locked` passed.
- Dashboard Node tests: 71 passed, including 21 display/detail regressions,
  browser capability lifetime checks and an actual isolated native HTTP setup
  test. The existing provider Node suite's 296 checks also passed.
- Provider inventory checks covered all 97 manifests and declared source modes.
  Shared icon checks passed for all 62 linked providers without changing the pin.
- Diff whitespace checks passed. No unrelated provider fixture or inventory
  regeneration was needed.

The native test used a private temporary profile and a synthetic local plugin.
It checked owner-key bootstrap, unauthorized session denial, loopback Host and
Origin validation, duplicate-header rejection, no setup CORS grant, write-only
credentials, preservation/explicit clearing of saved secrets, stale revision
rejection, unknown/structured field preservation and private file permissions.
T3/SDK-like keys did not authorize setup. Restart preserved the private setup
key but invalidated the launch capability. Ingestion-only mode exposed neither
setup nor a fabricated provider catalogue.

Saving provider-specific quota settings changed real native fixture probes
without restarting the process. Two configured sources used independent
snapshot/daily IDs. External configuration changes were detected automatically.
Pausing or removing a source preserved its history. Custom readout commands
retained supplied failure state/timestamps, while failed command output was not
exposed. These commands were fixture-only local `printf` readouts.

## Collaborative browser evidence

The rebuilt native daemon served an isolated profile at loopback port 6748 with
three explicitly labelled synthetic providers and 65 days of generated daily
data. No production configuration or provider account was attached.

Browser interaction and DOM checks confirmed:

- Each logo menu contains only its own provider family: Claude/Anthropic/Claude
  Code, Codex/OpenAI and Copilot/GitHub Copilot. The default is not duplicated.
  Imported unrelated icon selections are also ignored. Monochrome and colour
  remain independent style controls, including fallback for colourless logos.
- Logo fill, meter layout, reset formatting, theme and provider spacing apply.
  Explicit spacing also applies in compact density. Custom raster-logo upload
  and removal use only display preferences.
- The collection file picker bootstraps access using the private fixture setup
  file. A full page reload keeps only the launch capability in sessionStorage;
  restarting the fixture invalidates it and shows the file picker again.
  The key never enters display preferences. Provider secret inputs remain blank.
- Collection edits preserve display preferences. Pausing a source retains saved
  usage; new sources start paused. An unsaved form survives switching settings
  scopes and saving another form, without persisting drafts to browser storage.
- Quota arcs/meters expose exact used/limit/remaining values, state/source and
  reset/window details. Daily charts, calendars and cost stacks expose the
  available token/cost categories and partial/unpriced/source information.
  Historical quota charts retain their own timestamp data instead of borrowing
  current daily cost or tokens. Small costs retain precision.
- Chart focus and synthetic browser keyboard events inspect recorded points;
  tap opens details. The preview's hardware key tool failed, so these checks do
  not claim manual keyboard-device qualification. Missing quota samples stay
  gaps; failed observations do not become a zero or fresh available quota.
- Light/dark layouts were visually reviewed. Display, unlocked collection forms
  and the locked setup prompt fit a 390-pixel viewport without document-wide
  horizontal overflow. Tooltips stay inside the viewport and can scroll.

UsageStat Bar's stored settings hash remained
`dcf7ee986ac81e84bee3df9b6854b613a06e83df758fb7a29633d78ea3d2ff48`.
No Bar setting was changed. Browser artifacts and runtime logs are local
validation aids, not published provider/account reconciliation evidence.

## Installation and contract boundaries

The regular readback service at port 6736 remains the installed v2.0.0 RPM.
The separate SDK-owned stable ingestion daemon at port 7436 and RC.2 host at
7433 were not modified. Their credentials, grants, records and service ownership
remain outside this task. No execution, embedding, model call, account migration
or billing fallback was performed.

The new dashboard requires a subsequent native build/package. The existing RPM
upgrade integration remains in the prior main commit; this validation did not
install a new RPM or change the system timer. Native `/v1/usage` and durable
`agenticdriver.usage.v2` ingestion retain their established contracts. The new
owner capability protects only local collection setup; it is not an application
login or an SDK transport/auth migration.
