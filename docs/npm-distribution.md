# npm distribution implementation

Issue: #21. `@hashimkarim/usagestat@alpha` and all five native payload packages
are public at `2.0.0-alpha.1` as of September 9, 2026. The first release used the
owner's authorized local bootstrap and does not have CI provenance.

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

The manual `npm-rehearsal.yml` workflow also accepts `public_registry: true`
with an existing `release_tag`. Before installation it verifies the public
dist-tag, package metadata, downloaded tarball integrity and every file's bytes
against staged release contents. Tar metadata may differ between filesystems;
payload differences, links and duplicate paths fail verification. Public npm
installation uses an isolated config, cache and prefix, without account tokens.
This mode can run once all six packages are published; passing the isolated
registry rehearsal alone does not establish public registry availability.
Registry verification reads npm's installation metadata and the full exact-version
endpoint. A new package's overview may still return 404 while installation
metadata and its tarball are already public. Both metadata representations must
agree on the published integrity; full version fields such as `libc` remain checked.

The staged alpha installation rehearsal passed on all five native targets in
[run 34297460188](https://github.com/hashimkarim/usagestat/actions/runs/34297460188).
Public installation checks passed on all five targets in
[run 34329248371](https://github.com/hashimkarim/usagestat/actions/runs/34329248371).
Windows assembly explicitly retains LF source/generated text so strict public
payload comparison checks identical bytes on every runner.
The checks cover a stopped same-version reinstall. Login/reboot, real-account
coexistence and interrupted distinct-version upgrades remain open in #21/#20.
Follow the explicit state restoration and removal instructions in the package README.

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
read replicas, and checks that prereleases preserve each existing `latest` tag.
npm may assign a new package's only version to `latest` even with `--tag alpha`.
That initial default is allowed; there is no stable npm release yet. Installation
instructions explicitly select `@alpha`. Public verification permits this default
only while the alpha is the package's sole version. Future alphas must preserve
the prior default, and a stable publication will intentionally set `latest`.

All six packages have verified GitHub trusted publishers bound to owner
`hashimkarim`, repository `usagestat`, workflow `release.yml`, environment `npm`.
The registry grants direct and staged publication within that binding. The GitHub
`npm` environment allows deployment only from `v*` tags. The release job grants
`id-token: write` and requests provenance; no npm account token is stored in Actions.
CI publication requires both `publicationEnabled: true` in `npm/distribution.json`
and repository variable `NPM_PUBLISH_ENABLED=true`. Both gates are now enabled after successful public installation checks on all five
targets. Future release tags must contain this configuration and workflow; the
existing alpha was bootstrapped locally, so OIDC publication will first execute
on a subsequent release.

Sources: [npm metadata](https://docs.npmjs.com/cli/v11/configuring-npm/package-json/),
[trusted publishers](https://docs.npmjs.com/trusted-publishers/),
[Node 24 requirements](https://github.com/nodejs/node/blob/v24.x/BUILDING.md#platform-list),
[Node signals](https://nodejs.org/api/child_process.html#subprocesskillsignal).
Windows Node kill emulation is abrupt; console events get time for native
cleanup, and managed services use the native supervisor/authenticated control.
