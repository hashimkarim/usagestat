# npm distribution implementation

Issue: #21. The selected name is `@hashimkarim/usagestat`, with five
`@hashimkarim/usagestat-<platform>` payload packages. On September 9, 2026,
the alpha installation rehearsal passed on all five native targets in
[run 34297460188](https://github.com/hashimkarim/usagestat/actions/runs/34297460188).
First publication created the Linux x64 payload. The main package is not yet
published: npm rejects the saved token for dist-tag cleanup and trusted-publisher
configuration (HTTP 403), so a fresh account login is pending.
`npm/distribution.json` keeps automated `publicationEnabled` false.

The [package README](../npm/README.md) covers requirements, explicit service
ownership, updates and removal. Node 24/npm 11.5.1+ are required. Node raises the
npm macOS minimum to 13.5; Rust-only artifacts still target 11.0.

The assembler consumes the checked #14 artifact inventory, rejecting mixed
versions/commits/resources and dirty inputs. Platform packages retain native
siblings, plugins/icons and license notices. Exact-version optional dependencies
use npm `os`, `cpu`, and Linux `libc` selection. Launchers verify versions and
every native payload hash, then spawn without a shell. There are no install hooks,
downloaders or compiler requirements. Native service registration records the
actual sibling daemon, preserving Node-free login startup.

```sh
python tools/publish/scripts/npm_packages.py --artifacts NATIVE_INPUTS --output target/npm-packages
python tools/publish/scripts/npm_packages.py --pack target/npm-packages/npm-packages.json
python tools/portability/npm_install.py target/npm-packages/npm-packed.json
```

Rehearsal packages are private, preventing accidental publication. Stable staging
selects qualified targets; prerelease staging requires a prerelease version.
`npm pack` must match the file allowlist. The packed plan retains SHA-256/SHA-512
integrity and package metadata. The Release workflow tests installation through
a disposable registry on five native runners, including scripts-disabled global
installation, command shims, resources/doctor, daemon health and shutdown,
literal arguments, exit codes, local npm exec, omitted dependencies and retained
data after removal. All real providers are disabled before daemon startup.

The [five-target npm installation rehearsal](https://github.com/hashimkarim/usagestat/actions/runs/34079566610)
passed on September 7, 2026 using the exact staged packages from
[`385466e`'s release run](https://github.com/hashimkarim/usagestat/actions/runs/34078930473).
It includes native Windows `.cmd` invocation and rejection of service ownership
from the real temporary npm exec cache. The initial Windows failure was the
Python fixture's command-line quoting; the corrected fixture passed against the
same package bytes. Login/reboot, real-account coexistence, running/interrupted
version upgrades and first public-registry publication remain pending in #21/#20.

[Run 34080463278](https://github.com/hashimkarim/usagestat/actions/runs/34080463278)
also passed all five archive builds, aggregate verification and all five npm
installations at `68b5e20`. This regenerated the packages with runtime-only
resources, excluding provider tests while retaining Droid's shared Factory entry.
Publication jobs were skipped; no registry or GitHub release was created.

The same five-target rehearsal passed again with `3ae3be1`'s release binaries in
[run 34094784327](https://github.com/hashimkarim/usagestat/actions/runs/34094784327),
alongside native Homebrew and Windows installer tests. The npm checks cover a
stopped same-version reinstall; they do not establish an interrupted distinct-
version npm upgrade or public registry publication. Follow the explicit state
restoration and unregister-before-removal instructions in the package README.
All five npm installation jobs also passed with the final `354c390` candidate in
[run 34096637011](https://github.com/hashimkarim/usagestat/actions/runs/34096637011).

The publication helper checks every staged/existing version before uploading,
publishes and verifies platforms before the main package, and rejects conflicting
published bytes. Retries skip only matching integrity and metadata. It sets
`latest` for stable, `alpha` for numbered alphas, and `next` for other prereleases
directly during publication; npm's trusted publishing does not
automatically authenticate a separate `dist-tag` command.
It rejects any retry that would move a tag backwards. If identical bytes already
exist under a different tag, it stops before uploading and reports the explicit
authenticated promotion needed; it never rewrites an existing release version.

The explicit local `--publish --bootstrap` mode uses the authenticated namespace
owner for the first alpha packages, without claiming CI provenance. It keeps
the exact staged tarballs for partial-publication recovery, waits for registry
read replicas, and removes an automatically assigned `latest` tag only when
that tag names the same first alpha. Tag removal requires account authentication.

First publication completion and trust setup remain pending. CI publication requires both
`publicationEnabled: true` and repository variable `NPM_PUBLISH_ENABLED=true`.
Configure each npm package's trusted publisher with owner `hashimkarim`, repo
`usagestat`, workflow `release.yml`, environment `npm`, and direct publish
permission. The job grants `id-token: write` and requests provenance. Initial
package creation may require an authorized first publication before these
settings exist. Bootstrap login uses a separate private local npm config, preserving
the existing credential configuration.

Sources: [npm metadata](https://docs.npmjs.com/cli/v11/configuring-npm/package-json/),
[trusted publishers](https://docs.npmjs.com/trusted-publishers/),
[Node 24 requirements](https://github.com/nodejs/node/blob/v24.x/BUILDING.md#platform-list),
[Node signals](https://nodejs.org/api/child_process.html#subprocesskillsignal).
Windows Node kill emulation is abrupt; console events get time for native
cleanup, and managed services use the native supervisor/authenticated control.
