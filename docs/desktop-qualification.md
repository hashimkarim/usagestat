# Desktop qualification handoff

Tracker: [#20](https://github.com/hashimkarim/usagestat/issues/20).
The native backend, service adapters, native packages and npm launchers have
automated evidence on all five initial targets. This document records the
remaining installed desktop checks needed to finish the open implementation
issues. [Native validation](native-validation.md) links the complete CI evidence;
[platform support](platform-support.md) defines the candidate OS floors.

## Inputs still needed

| Input | Work it unlocks |
| --- | --- |
| A Windows x64 desktop and macOS Intel/Apple Silicon desktops, with a disposable normal user who can log out and reboot | #8/#9/#16: actual login, power, console, permissions and installation acceptance |
| Minimum-version machines or a revised supported-OS decision | #15/#16/#20/#21: qualify the advertised floors; a new CI image does not establish an older OS |
| Consenting test accounts and installed provider/browser/IDE versions | #10/#11/#12/#18/#19: real credential mappings, prompts, refresh and usage results |
| Native bar builds from [Windows bar #14](https://github.com/hashimkarim/usagestat-bar/issues/14) and [macOS bar #15](https://github.com/hashimkarim/usagestat-bar/issues/15) | #17: actual frontend discovery, lifecycle and display acceptance |
| Apple signing/notarization identity and a Windows publisher/signing policy | #15/#16/#20: prepare and test the intended public trust path |
| First npm publication setup, trusted publishers for all six packages, and an approved new release version | #21: public registry installation and provenance qualification |

The repository readiness check on 2026-09-07 found no configured GitHub
environments, no `NPM_PUBLISH_ENABLED` variable, and no Apple/Windows signing or
npm publication secrets among the repository secret names. Existing Linux/tap
publisher secrets were present. This read-only inventory does not establish the
absence of organization secrets, local certificates or npm account permissions.
No account settings or credentials were changed. npm publication remains disabled
by the committed distribution plan and the workflow gate.

## Select and install the candidate

Use a fresh OS test user so the default service identity does not collide with an
existing installation. Start with synthetic settings and disabled providers. Keep
the exact release run, commit, target manifest, archive checksum and installer
checksum with the result. Download the `usagestat-complete-release-inputs` artifact
from the selected successful rehearsal; a CI artifact is not a public release.

Follow [Windows installation](windows-distribution.md) for the verified per-user
PowerShell installer, or [macOS distribution](macos-distribution.md) for native
archives and the Homebrew candidate procedure. First installation must discover
both executables, all provider icons and licenses without registering startup.
Verify from an unrelated working directory, including a path with Unicode/spaces.
On Windows, run without administrator rights, WSL, Python, Node or a Rust compiler.
The separate npm route intentionally requires the documented Node/npm versions.

Record `usagestat --version`, `usagestat capabilities --json`, `usagestat doctor
--json`, and `usagestat daemon status --json` locally. Diagnostics can contain local
paths; review and redact them before attaching evidence. Do not attach configuration,
credential files, browser databases, management keys or auth-import JSON.

## Installed acceptance matrix

Run each applicable row on each advertised OS/CPU. Record `passed`, `failed`,
`blocked` or `not-applicable` with an explanation; an unrun row is not a pass.
Test the minimum and current claimed OS separately. The candidate targets remain
macOS Intel/ARM64 and Windows x64; npm's macOS floor also includes Node's floor.

| Scenario | Expected result | Owning issues |
| --- | --- | --- |
| First install, enable twice, status, disable twice | No implicit startup; one owned healthy daemon after enable; disabled state retains config/T3/keys | #8/#9/#15/#16/#21 |
| Running/autostart combinations: on/on, off/on, on/off, off/off | `daemon start`/`stop` preserve startup; `daemon autostart on`/`off` preserve current running state | #8/#9 |
| Logout/login and reboot | Enabled daemon returns as the same normal user with correct saved resources/environment; disabled startup remains off; Windows has no unexpected console window | #8/#9 |
| Crash, helper timeout, offline/recovery, sleep/resume | Owned helpers leave no orphan processes; daemon recovers with retained data; independent providers continue to report their own states | #8/#9/#11/#12/#20 |
| Missing/denied/locked native credentials | Distinct actionable auth result; no wrong-account fallback, lost metadata or secret output; unrelated providers still work | #10/#18/#19 |
| Port or service-owner conflict | Existing process/registration is preserved; explicit owner transfer is required | #8/#9/#17 |
| Native local redirected folders, Unicode/spaces, minimal PATH, Windows HOME absent | Native folder resolution and saved helper/resource paths work; no cwd-dependent state or shell parsing | #11/#12/#16 |
| Dev and release together | Different executable/profile/service identities and ports; neither installation mutates the other's state | #8/#9/#16/#17 |
| Distinct-version upgrade, healthy and deliberately failing candidate | Correct new version/resources on success; retained previous version and independent running/startup/T3/data state on recovery | #15/#16/#21 |
| Interrupted replacement and Windows external file lock | Actionable retained journal/backup; recovery after resolving the lock/session; no unrelated file removal or process termination | #15/#16/#20 |
| Unregister and uninstall twice | Owned login entry and payload removed, retained user data/keys/preferences and unrelated installations preserved | #15/#16/#21 |
| Signed download from a browser | Expected publisher/team, signing/notarization checks and normal Gatekeeper/Windows policy behavior on a clean machine | #15/#16/#20 |
| Native bar plus bundled, Homebrew/native and global npm backends | Explicit/discovered executable works, capability states render correctly, no duplicate owner, reconnect after upgrade | #17/#21 |

For upgrade tests, use two distinct verified backend releases and retain the old
payload until health is established. Homebrew uses retained kegs plus `daemon
relocate`/`daemon recover`. Windows uses the installer's `Install`/`Recover`
transaction. npm uses the explicit stop, exact-version install and state-restoration
sequence in [its README](../npm/README.md); it does not provide an automatic npm
transaction. The current Homebrew revision and Windows same-build replacement
fixtures exercise their transaction paths but do not prove a distinct-version
upgrade, package data migration or a real desktop session.

## Real provider and browser checks

Select accounts explicitly before using them. Keep results scoped to an exact OS,
provider/app version, source and credential method. The generated
[provider inventory](provider-compatibility.md) is the source of current
implemented, partial and unsupported classifications; do not infer live support
from a synthetic fixture.

| Representative source | Acceptance |
| --- | --- |
| OpenAI API key | Correct usage with appropriate account permissions over real TLS; denied permissions and offline results remain distinct |
| Codex local logs and OAuth | Selected canonical profile, direct/encrypted credential method, account identity and supported refresh persistence; see [Codex auth](codex-authentication.md) |
| Claude local logs and native auth | Selected profile, macOS Keychain consent/deny/locked behavior, supported Windows source and refresh |
| Cursor and another supported IDE | Correct stable/nightly/custom database and current-user language-server process; ambiguous sessions require selection; live/WAL databases remain unchanged |
| Browser-backed provider with manual auth | Provider-supported manual/cURL/OAuth route succeeds, expiration/challenge yields useful guidance, secrets stay out of logs |
| Chrome/Brave/Chromium automatic import | Exact original-user profile, current browser/schema/encryption format, scoped cookies and prompt/deny handling; see [browser auth](browser-authentication.md) |

Windows App-Bound `v20` import must return `APP_BOUND_UNSUPPORTED` before a key
lookup. Qualify the provider's documented manual/OAuth/API alternative separately;
do not bypass browser protections or count a failed import as a working fallback.
Record unsupported future schemas explicitly rather than expanding the claim.

## Record and close

Attach a sanitized table to the owning issue using these columns:

| Candidate commit and SHA-256 | OS build / CPU / user type | Install channel and version pair | Scenario / provider / source / app version | Result | Evidence or blocker |
| --- | --- | --- | --- | --- | --- |
| Fill from the selected manifests | Record actual host | Include old and new versions for upgrades | One independently assessed case per row | Pending | Sanitized log, screenshot or exact missing input |

For npm, also record Node/npm versions, the six published package versions and
integrities, registry/dist-tag/provenance results, scripts-disabled installation
and actual native bar discovery. Follow [npm distribution](npm-distribution.md)
for the already prepared publishing gates and initial trusted-publisher setup.

Close an issue only when its acceptance criteria have evidence. Update package
qualification metadata, provider classifications and installation claims together.
Keep unverified OS minima and provider methods out of the published support
promise. Select a new release version before staging public publication; do not
overwrite an existing version's assets or npm bytes with the rehearsed candidate.
