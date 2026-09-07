# macOS backend distribution

Implementation and acceptance tracker: [#15](https://github.com/hashimkarim/usagestat/issues/15).
Unsigned **v2.0.0-alpha.1** archives are covered in the [alpha installation guide](alpha.md).
Stable macOS packages remain pending public qualification. The currently published
Homebrew formula supports Linux. The existing owned tap is
[`hashimkarim/homebrew-tap`](https://github.com/hashimkarim/homebrew-tap).

[Release rehearsal 34094784327](https://github.com/hashimkarim/usagestat/actions/runs/34094784327)
at `3ae3be1` passed both native Homebrew installation/active-revision-upgrade/
recovery/removal jobs, all five archive builds and all five npm installation
rehearsals. Both Homebrew reports contain all nine checks. Publication jobs were
skipped; signed distribution and normal desktop acceptance remain pending.
The same complete rehearsal passed again at `354c390` in
[run 34096637011](https://github.com/hashimkarim/usagestat/actions/runs/34096637011),
including the retained-executable fingerprint check during daemon recovery.

The release workflow builds separate Intel and Apple Silicon archives containing
`usagestat`, `usagestatd`, bundled plugins/icons and license notices. No Node or
Rust installation is needed for the native archive/Homebrew payload. npm is a
separate distribution path with its own Node requirement; see [npm distribution](npm-distribution.md).

`tools/publish/scripts/homebrew_formula.py` reads the verified aggregate and
per-target manifests, validates hashes, contained file contents, native binary
architecture and shared resources, then generates the architecture branches.
The stable formula includes only targets with `minimumSystemQualified: true`,
requires both CPUs for each included OS, and retains Linux-only restrictions
while Mac qualification is pending. It never adds guessed checksums or URLs to
the existing published version. `prepare-packages.py` uses this generator for
manifest-based releases and retains the Linux adapter for older releases.

The formula installs both executables in its keg's `bin` and resources in
`share/usagestat`. Homebrew exposes durable `opt/usagestat` and `bin` links.
The CLI exclusively owns login registration; there is no `brew services` stanza
and installation does not enable startup. The bar should discover an explicit
CLI path or a durable installed link, then use the backend's capabilities and
saved owner. A bundled desktop backend has a different owner and requires an
explicit owner switch before taking over an existing profile.

For development, `python tools/portability/stage_dev.py --output target/native-dev-first`
builds/stages both renamed dev executables and resources in a fresh directory.
It uses the `usagestat-dev` profile without changing a login registration or PATH.
See the [portable development workflow](windows-distribution.md#native-development-build).

For a disposable CI rehearsal, the same generator can produce local `file:` URLs:

```sh
python tools/publish/scripts/homebrew_formula.py --artifacts dist --output target/usagestat-local.rb --rehearsal
```

That output is an unsigned local test recipe and must not enter the public tap.
The release workflow runs `homebrew_rehearsal.py` on macOS Intel and Apple Silicon.
It creates one uniquely named local tap/formula, installs and tests the real
payload, checks linked executable/resource discovery, repeats installation,
upgrades a Homebrew revision into a new keg, and removes its formula/tap while
checking retained synthetic user data. It refuses to replace an existing linked
backend or login agent and requires a disposable hosted Mac runner. The current
expanded rehearsal registers the otherwise absent agent with an isolated HOME,
config/data profile and no enabled provider. It upgrades four retained Homebrew
revisions and uses `daemon relocate` to preserve every running/autostart combination.
A deliberately failing fixture executable verifies rollback; killing a relocation
command verifies the private journal and `daemon recover`. These expanded checks
passed in the release run linked above. Results are uploaded as `homebrew-rehearsal-*`;
failures block the release workflow. Native CI separately tests the LaunchAgent
adapter with an isolated service identity.

Public installation commands will be `brew install hashimkarim/tap/usagestat`,
`brew upgrade hashimkarim/tap/usagestat` and
`brew uninstall hashimkarim/tap/usagestat` once the Mac formula is qualified and
published. Before removing a managed installation, use `usagestat daemon unregister`
to stop/remove its owned LaunchAgent while retaining saved intent and keys.
Uninstall retains configuration/history/credentials. Normal desktop and
bar/Homebrew coexistence acceptance remains pending. Rehearsal upgrades use actual
Homebrew revisions of one verified backend build; upgrades between different
backend versions still need release-pair qualification.

For an already registered Homebrew daemon, retain its previous keg during the
package upgrade, then relocate the registration from the newly selected CLI:

```sh
HOMEBREW_NO_INSTALL_CLEANUP=1 brew upgrade hashimkarim/tap/usagestat
"$(brew --prefix usagestat)/bin/usagestat" daemon relocate
```

Use the actual selected formula name if testing a local fixture. Installation
without a registered daemon does not need relocation; opt into startup separately
with `daemon enable`. On success, saved binary and bundled plugin paths point to
the new keg; paused or autostart-disabled states, custom provider/config/key paths,
T3 intent and environment remain unchanged. Repeating relocation is safe.

If relocation fails, the previous retained executable and settings are restored.
An interrupted command leaves `daemon-relocation.json` beside the private settings;
retain both kegs and run the selected CLI's `daemon recover`. Recovery refuses
unrelated settings/owner changes or a missing/changed previous executable. It does
not change Homebrew's selected formula or remove its kegs. If the daemon has rolled
back to an older backend version, use that retained keg's matching CLI for the bar
until the package issue is resolved. Run `brew cleanup usagestat` only after a
successful relocation, when keeping the previous package is no longer needed.
Raw in-place/npm replacements use their owning installer's restart/rollback flow.

Re-enabling a relocated installation now replaces recognized old bundled plugin
paths with locations derived from the selected new daemon executable. Custom
plugin paths keep their precedence; matching still works after the old keg was
removed. Explicit owner transfers apply the new archive/app resource layout.
The recoverable `daemon relocate` operation uses this mapping after the package manager has installed and verified the new keg.

Direct desktop signing belongs to the actual macOS bar bundle, whose frontend
is tracked in [bar #15](https://github.com/hashimkarim/usagestat-bar/issues/15).
Its handoff must include both backend executables and matching resources, sign
nested Mach-O executables before the outer app with Developer ID/hardened runtime,
verify the expected Team ID and architectures, obtain notarization acceptance,
staple the app, and create the final archive afterward. QuickJS interprets code;
these builds have not established a need for JIT or unsigned-executable-memory
entitlements. Do not add those entitlements without a measured runtime need.
Raw standalone executables cannot carry stapled notarization tickets. The current
archives are explicitly unsigned and do not pass a signed desktop release gate.

Remaining external readiness is an existing Developer ID Application certificate
and private key for the selected Apple team, its identity fingerprint, an
authorized notarization API key/issuer, and a clean Mac for Gatekeeper/login tests.
These values are not embedded in this repository, and this setup does not enroll
an account or publish a release. Candidate native minimum macOS 11 is still
unqualified; Homebrew/npm may require newer host versions. Record actual installed
OS/CPU, app version, signing outcome and login/reboot results in #20.

References: [Homebrew formula platform branches and installation](https://docs.brew.sh/Formula-Cookbook),
[Homebrew tap/test/upgrade commands](https://docs.brew.sh/Manpage),
[Apple notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
