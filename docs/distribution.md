# Release distribution

The native npm distribution is being prepared in #21. See
[npm packaging and release setup](npm-distribution.md) for staged packages,
scripts-disabled installation tests and pending first-publication setup.

This page is for maintainers. For published package availability and user-facing
install, upgrade, and removal commands, see the [installation guide](installation.md).

## Automatic publishing

Stable `vMAJOR.MINOR.PATCH` tags now run the complete release pipeline:

1. Run the workspace tests and build all five initial native targets.
2. Validate extracted archives and publish eligible GitHub assets and checksums.
3. Publish AUR, Homebrew, Fedora COPR, and the Ubuntu PPA in independent jobs.

Explicit `vMAJOR.MINOR.PATCH-alpha.NUMBER` tags additionally publish to separate
alpha destinations listed in [alpha-targets.json](../packaging/alpha-targets.json):
`usagestat-alpha-bin` in AUR, `usagestat-alpha` in the owned Homebrew tap, and
`hashimkarim/usagestat-alpha` in COPR and Launchpad (RPM/Debian package `usagestat`).
Other prerelease forms remain on GitHub. Package publishing validates
that the requested tag is the latest published stable release, verifies both
archive checksums and architectures, and generates recipe versions/checksums
from those downloads. A missing credential fails the affected job explicitly;
it does not silently skip a repository or prevent the other repositories from
publishing.

Alpha publication requires the newest numeric alpha version, an existing public
GitHub prerelease and matching immutable source/tag/assets. RPM and Debian use
`2.0.0~alpha.1`; AUR uses `2.0.0alpha.1`; all programs report the upstream
`2.0.0-alpha.1`. Stable validation remains strict and cannot accept an alpha tag.
The alpha Homebrew formula explicitly includes the unsigned macOS candidates;
the stable formula retains its existing qualification gate.

The native artifact workflow and schema are documented in
[native release artifacts](native-artifacts.md). Manual Release workflow dispatch
stages all targets without publishing. Stable publication retains Linux while
macOS/Windows minimum-system qualification is pending; prereleases may include
the unsigned desktop candidates. Existing release assets are compared against
the tested source and hashes before any upload; different or incomplete releases
stop for reconciliation rather than being overwritten.

### One-time GitHub Actions configuration

Set these **repository secrets** on `Hashim-K/usagestat`:

| Secret | Value |
| --- | --- |
| `AUR_SSH_PRIVATE_KEY` | Unencrypted SSH private key registered to the AUR maintainer of `usagestat-bin`. |
| `HOMEBREW_SSH_PRIVATE_KEY` | Unencrypted SSH private key whose public key is a **write-enabled deploy key** on `hashimkarim/homebrew-tap`. |
| `COPR_CONFIG` | Complete COPR API configuration, including the `[copr-cli]` section, login, token, and username. |
| `PPA_GPG_PRIVATE_KEY` | ASCII-armored private signing key registered to the Launchpad PPA uploader. |
| `PPA_GPG_PASSPHRASE` | Signing-key passphrase, if the exported key is protected; otherwise omit. |

Set these **repository variables**:

| Variable | Value |
| --- | --- |
| `AUR_SSH_KNOWN_HOSTS` | Verified `aur.archlinux.org` SSH known-hosts entry. Strict host verification stays enabled. |
| `HOMEBREW_TAP_REPOSITORY` | `hashimkarim/homebrew-tap` (GitHub `OWNER/REPOSITORY`, without a URL prefix or `.git` suffix). |
| `PPA_GPG_FINGERPRINT` | Full fingerprint of the imported Launchpad signing key. |

Create new AUR SSH and Launchpad OpenPGP keys alongside existing account keys;
do not replace them. Launchpad requires the new public key to be published to
its keyserver and confirmed on the uploader account. A CI signing subkey can
be exported without exposing the primary private key.

COPR has one API token per account: `copr-cli new-api-token` invalidates the
existing token. For an independent CI credential, use a separate Fedora
account granted builder access to `hashimkarim/usagestat`. Its configuration
still belongs in `COPR_CONFIG`; the project owner does not change. Reusing an
existing token does not rotate it, but should be an explicit choice.

Use dedicated publishing credentials where possible. The Homebrew key only
needs access to the tap, not a personal GitHub account token. Secrets are kept
out of package artifacts and Git remotes; temporary key files are removed on
exit. Dry runs do not upload packages or require publishing credentials.

### Retry an existing release

In **Actions → Publish package repositories → Run workflow**, select `main`,
enter the existing stable tag, choose `all` or one repository, and set **dry_run**
to false to publish. For example:

```bash
gh workflow run publish-packages.yml --ref main \
  -f tag=v1.0.3 -f platform=all -F dry_run=false
```

Leave `dry_run=true` for a rehearsal. This supports publishing a release made
before the automation was added, without moving its tag or rebuilding its
GitHub assets. AUR and Homebrew commit only changed recipes. COPR waits for an
existing build, or submits a new build if no successful attempt exists. The PPA
waits for existing uploads and for the resulting binaries to be published;
already used PPA versions cannot be uploaded again after a failed build. Retry
that build in Launchpad, or change the Debian package revision deliberately.

Repository jobs are serialized per platform and have a 75-minute timeout.
Remote build/publishing waits have a 45-minute timeout. A timeout reports a
failure; rerunning the job checks the existing remote state first. Generated
recipes and Debian source packages are retained as workflow artifacts.

For the existing alpha, dispatch the implementation branch until this workflow
has been integrated into `main`:

```sh
gh workflow run publish-packages.yml --ref feat/native-backend-implementation \
  -f tag=v2.0.0-alpha.1 -f channel=alpha -f platform=all -F dry_run=true
```

After the validation run succeeds, use `dry_run=false` to publish the same
existing release. Select one platform to retry it. Concurrency is separate for
each channel/platform. Existing credentials are reused; alpha setup does not
rotate keys or modify stable repositories.

The AUR package targets x86-64. Homebrew supports Linux x86-64 and ARM64. COPR's
v1.0.3 build covers Fedora 43, 44, 45, and Rawhide on x86-64. The PPA's published
v1.0.3 binary targets `noble` (Ubuntu 24.04) on amd64. The source recipe also
contains an ARM64 payload, but ARM64 should only be advertised once a binary is
published for that architecture. The default PPA version suffix is `-1ppa1`.
The v1.0.3 release binaries require glibc 2.39 on both architectures. Homebrew
and manual-download instructions must retain that system compatibility limit
until the release build baseline changes.
Version 1.0.3 uses `-1ppa2` to replace a package rejected for epoch-zero file
timestamps. The Debian install step sets bundled timestamps from the package
changelog; the reproducible upstream source archive remains unchanged. Uploads
and publication checks share the revision in `publication-state.py`.
As in the existing PPA, its Debian source package wraps the validated release
binaries and plugins. Both the CLI and daemon are installed. It does not run
Cargo on Launchpad.

## Local-first publishing

Publishing should be rehearsed locally before pushing a release tag or
submitting to package repositories.

Build the local release artifacts exactly like the GitHub Release workflow:

```bash
tools/publish/scripts/local-release-build.sh 1.0.0
```

Artifacts are written to:

```text
dist/releases/v1.0.0/
```

Build local distro packages:

```bash
tools/publish/scripts/local-deb-build.sh
tools/publish/scripts/local-rpm-build.sh
```

Both package builders install the resulting package inside the publisher
container and run:

```bash
usagestat --version
usagestat test https
```

Packages are written to:

```text
dist/packages/v1.0.0/
```

Build publisher containers:

```bash
tools/publish/scripts/docker-build.sh arch-aur
tools/publish/scripts/docker-build.sh fedora-copr
tools/publish/scripts/docker-build.sh ubuntu-ppa
```

Open a shell in a publisher container:

```bash
tools/publish/scripts/docker-run.sh arch-aur
tools/publish/scripts/docker-run.sh fedora-copr
tools/publish/scripts/docker-run.sh ubuntu-ppa
```

Optional credential mounts:

```bash
MOUNT_SSH=1 tools/publish/scripts/docker-run.sh arch-aur
MOUNT_COPR=1 tools/publish/scripts/docker-run.sh fedora-copr
MOUNT_GNUPG=1 tools/publish/scripts/docker-run.sh ubuntu-ppa
```

## GitHub Releases

Push a semver tag prefixed with `v`:

```bash
git tag v1.0.0
git push origin v1.0.0
```

The release workflow builds both `usagestat` and `usagestatd` with `cross` for:

- `x86_64-unknown-linux-gnu` -> `usagestat-linux-x86_64`
- `aarch64-unknown-linux-gnu` -> `usagestat-linux-aarch64`

Each release includes `.sha256` checksum files. The `.tar.gz` archives contain
both binaries, plugins, and the license; the standalone downloads contain only
the CLI. See [direct downloads](installation.md#direct-release-downloads).

## AUR

Use `packaging/aur/usagestat-bin/PKGBUILD` for the binary package. Before publishing:

1. Set `pkgver` to the release version without the leading `v`.
2. Set `sha256sums_x86_64` to the checksum from `usagestat-linux-x86_64.tar.gz.sha256`.
3. Validate in the Arch container:

```bash
tools/publish/scripts/aur-check.sh
```

4. Run `makepkg --printsrcinfo > .SRCINFO`.
5. Commit `PKGBUILD` and `.SRCINFO` to the `usagestat-bin` AUR Git repository.

A separate source-build `usagestat` AUR package should wait until the CLI crate is published to crates.io or a source release tarball with vendored dependencies is produced.

## Homebrew

The existing tap is `github.com/hashimkarim/homebrew-tap`. Automated publishing reads
its destination from the `HOMEBREW_TAP_REPOSITORY` repository variable, which the
workflow passes into the publishing script. Set it before enabling publication:

```bash
gh variable set HOMEBREW_TAP_REPOSITORY --repo Hashim-K/usagestat \
  --body hashimkarim/homebrew-tap
```

The credential check and publishing script both require this variable; there is
no hardcoded fallback. Local publication also needs it exported in the environment.
Dry runs validate the recipe and binaries without a tap variable or publishing key.
When using linux-deploy doctor, use its default `homebrew.tap_source: "variable"`
with `homebrew.tap` set to `hashimkarim/homebrew-tap`.

For manual publishing, copy `packaging/homebrew/Formula/usagestat.rb` to
`Formula/usagestat.rb`.

For each release, update:

- `version`
- Linux x86_64 tarball `sha256`
- Linux aarch64 tarball `sha256`

See [Homebrew installation and formula trust](installation.md#homebrew-on-linux).

## COPR

Use `packaging/rpm/usagestat.spec` as the starting spec for COPR package `hashimkarim/usagestat`.

Submit a COPR build from the Fedora publisher container:

```bash
tools/publish/scripts/copr-build.sh hashimkarim/usagestat packaging/rpm/usagestat.spec
```

The current spec builds from the GitHub tag archive and enables network access
for Cargo dependency download. Stricter production builds should either vendor
Rust dependencies into the source package or use Fedora Rust packaging macros
generated by `rust2rpm` so builds do not depend on network access during
`%build`.

See [Fedora installation](installation.md#fedora--copr), including the DNF5
plugin prerequisite and currently published targets.

## Ubuntu PPA

The CLI crate has `cargo-deb` metadata for generating a local `.deb`:

```bash
cargo install cargo-deb
cargo build --release --locked -p usagestat-cli -p usagestat-daemon
cargo deb -p usagestat-cli --no-build
```

Launchpad PPAs require signed Debian source packages, not just binary `.deb`
uploads. Use the Ubuntu publisher container so the Debian toolchain is
consistent:

```bash
tools/publish/scripts/ppa-shell.sh
```

Inside the shell, prepare a Debian source package with a signed `.dsc` and
upload it with `dput`.

See [Ubuntu installation](installation.md#ubuntu--ppa), including repository
setup prerequisites, the supported series, and published architecture.
