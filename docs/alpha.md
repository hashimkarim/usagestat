# Try the v2 backend alpha

[v2.0.0-alpha.1 downloads](https://github.com/hashimkarim/usagestat/releases/tag/v2.0.0-alpha.1)
contain the CLI, daemon, local web dashboard/API and provider plugins for Windows
x64, macOS Intel/Apple Silicon and Linux x64/ARM64. This is an early backend
release; the Windows/macOS bar frontends are not included. Choose alpha for testing;
existing stable Linux packages remain available separately.

Windows/macOS binaries are unsigned and the Mac binaries are not notarized.
Use the exact repository release and matching checksums. Signing, normal desktop
login/reboot and real provider authentication still need qualification; see the
[release notes](releases/v2.0.0-alpha.1.md). Native CI runs on macOS 15 and Windows
Server 2025. Older candidate OS floors remain unverified. Linux requires glibc
2.39+; Windows ARM64 and musl/Alpine have no alpha payload.

## Alpha package repositories

Alpha packages are published separately from the stable repositories. Availability
was checked on September 9, 2026; pending channels are explicitly marked below.

| Channel | Alpha destination | Package / formula | Target |
| --- | --- | --- | --- |
| Fedora | [COPR](https://copr.fedorainfracloud.org/coprs/hashimkarim/usagestat-alpha/) | `usagestat` | Fedora 43, 44, 45, Rawhide; x86-64 |
| Ubuntu | [Launchpad PPA](https://launchpad.net/~hashimkarim/+archive/ubuntu/usagestat-alpha) | `usagestat` | Ubuntu 24.04 Noble; amd64; build finished, repository publication pending |
| Arch Linux | [AUR](https://aur.archlinux.org/packages/usagestat-alpha-bin) | `usagestat-alpha-bin` | x86-64; glibc 2.39+ |
| Linux / macOS | [Homebrew](https://github.com/hashimkarim/homebrew-tap/blob/main/Formula/usagestat-alpha.rb) | `hashimkarim/tap/usagestat-alpha` | Linux x86-64/ARM64; macOS Intel/Apple Silicon |
| Windows | [Scoop bucket](https://github.com/hashimkarim/scoop-bucket/blob/main/bucket/usagestat-alpha.json) | `usagestat-alpha` | Windows x64; published and installation tested |
| Windows | [Chocolatey submission](https://community.chocolatey.org/packages/usagestat/2.0.0-alpha000001) | `usagestat --pre` | Windows x64; submitted, awaiting community review |
| Windows | [WinGet submission](https://github.com/microsoft/winget-pkgs/pull/431692) | `HashimKarim.UsageStat.Alpha` | Windows x64; native tests and CLA passed, upstream validation/review pending |
| All five native targets | npm | `@hashimkarim/usagestat@alpha` | Installation rehearsals passed; main package publication awaits npm's publishing approval |

Ubuntu's signed source upload was accepted and the
[Noble build](https://launchpad.net/~hashimkarim/+archive/ubuntu/usagestat-alpha/+build/33580662)
finished successfully. Launchpad remains at “Uploading build”; no binary package
is published yet. The Ubuntu commands below become usable after that completes.

Alpha packages install the same `usagestat` and `usagestatd` commands as stable.
Switching channels replaces the package; these are not separate daemon profiles.
If a backend is already running, stop it with its current CLI's `daemon stop`
before replacing the package. Keep a backup of settings/history before trying an
alpha. Startup remains explicit; package installation does not enable a service.

Fedora installation:

```sh
sudo dnf install dnf5-plugins
sudo dnf copr enable hashimkarim/usagestat-alpha
sudo dnf install usagestat
usagestat --version
```

Ubuntu 24.04:

```sh
sudo apt install software-properties-common
sudo add-apt-repository ppa:hashimkarim/usagestat-alpha
sudo apt update
sudo apt install usagestat
usagestat --version
```

Arch users with an AUR helper can run `yay -S usagestat-alpha-bin`. For a
helper-free installation, clone
`https://aur.archlinux.org/usagestat-alpha-bin.git`, review `PKGBUILD`, and run
`makepkg -si` as your regular user. Accept replacement of the stable package only
when you intend to switch channels.

Homebrew installation is `brew install hashimkarim/tap/usagestat-alpha`.
If the stable formula is installed, stop/unregister its owned daemon and run
`brew uninstall usagestat` before installing the alpha formula. The new formula
uses a different Cellar path, so register startup explicitly again if wanted.
If Homebrew requires formula trust, review the generated formula and run
`brew trust --formula hashimkarim/tap/usagestat-alpha`; do not disable trust checks.
The macOS binaries remain unsigned, with desktop/minimum-OS qualification pending.

Update with `sudo dnf upgrade usagestat`, `sudo apt install --only-upgrade
usagestat`, or `yay -Syu usagestat-alpha-bin`.
Stop a running backend first, then use `usagestat daemon start` afterward if it
was previously configured. Homebrew upgrades change the Cellar path: use
`HOMEBREW_NO_INSTALL_CLEANUP=1 brew upgrade usagestat-alpha`, then
`usagestat daemon relocate` before cleaning the old keg. Restore the previous
running/autostart state afterward; see
[Homebrew upgrade recovery](macos-distribution.md).

Before removing a managed backend, run its `usagestat daemon unregister`.
Uninstall with `sudo dnf remove usagestat`, `sudo apt remove usagestat`,
`sudo pacman -R usagestat-alpha-bin`, or `brew uninstall usagestat-alpha`.
Settings/history are retained. Optionally remove only the alpha repository with
`sudo dnf copr remove hashimkarim/usagestat-alpha` or
`sudo add-apt-repository --remove ppa:hashimkarim/usagestat-alpha`.
Disabling an alpha repository alone does not downgrade its installed package.
To return to stable, unregister the alpha backend, remove its package, disable
the alpha repository and install from the [stable channel](installation.md).
Restore the pre-alpha settings backup if the older backend needs it.

### Windows package managers

With [Scoop installed](https://scoop.sh/), run:

```powershell
scoop bucket add hashimkarim https://github.com/hashimkarim/scoop-bucket
scoop install hashimkarim/usagestat-alpha
usagestat --version
usagestat --json list
usagestat doctor
```

Scoop installs the complete ZIP and exposes the CLI and daemon commands. It does
not register login startup. To opt in, run `usagestat daemon enable` after a
durable installation. Keep one installation owner per backend profile; unregister
an old portable or npm owner before intentionally switching to Scoop.

Before an update, record `usagestat daemon status --json`, including `running`
and `autostart`, then run `usagestat daemon disable` if configured. Run
`scoop update usagestat-alpha`, followed by `usagestat daemon relocate` for a
registered installation. Restore the saved state: turn autostart on only if it
was previously on, and run `usagestat daemon start` only if it was running.
Keep the previous Scoop version until this succeeds. Distinct-version desktop
upgrades still need acceptance testing.

Before removal, run `usagestat daemon unregister`, then
`scoop uninstall usagestat-alpha`. User settings and history are retained.

Chocolatey's [package naming rule](https://docs.chocolatey.org/en-us/community-repository/moderation/package-validator/rules/cpmr0024/)
requires the ID `usagestat`, with alpha selected through `--pre`. It uses
`2.0.0-alpha000001` because its community feed requires SemVer 1 prerelease
syntax; the binaries still report `2.0.0-alpha.1`. After community review,
installation will be `choco install usagestat --pre` in an elevated shell.
Updates use `choco upgrade usagestat --pre`; release Windows file
locks with `daemon disable` first and restore the saved daemon state afterward.
Removal uses `daemon unregister`, then `choco uninstall usagestat`.
Run backend setup commands from the regular user's shell so startup belongs to
the intended user. Chocolatey's community metadata validator, installation,
removal and upload passed in
[run 34305309448](https://github.com/hashimkarim/usagestat/actions/runs/34305309448);
community review approval is a separate step.

WinGet's native installation, resource and removal tests passed in
[run 34298557248](https://github.com/hashimkarim/usagestat/actions/runs/34298557248).
The owner's CLA response was accepted. Upstream installer validation and review
are still pending. WinGet commands will be added once accepted. These packages
contain the unsigned backend; package-manager availability does not qualify
desktop login, provider authentication, signing, or the bar frontend.

## Windows: portable first run

Download `usagestat-windows-x86_64.zip` and its `.sha256` sidecar into the same
directory. In Windows PowerShell, from that directory:

```powershell
$archive = 'usagestat-windows-x86_64.zip'
$checksum = (Get-Content -Raw ($archive + '.sha256')).Trim() -split '\s+'
if ($checksum.Count -ne 2 -or $checksum[1] -cne $archive -or
    $checksum[0] -notmatch '^[0-9a-f]{64}$' -or
    (Get-FileHash $archive -Algorithm SHA256).Hash -ine $checksum[0]) {
    throw 'Archive checksum mismatch.'
}
$destination = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\usagestat-v2-alpha.1'
if (Test-Path -LiteralPath $destination) { throw 'Choose a fresh extraction directory.' }
Expand-Archive -LiteralPath $archive -DestinationPath $destination
& (Join-Path $destination 'usagestat.exe') --version
& (Join-Path $destination 'usagestat.exe') --json list
& (Join-Path $destination 'usagestat.exe') doctor
```

Expect version `2.0.0-alpha.1` and provider manifests with usable icon paths.
These commands do not register startup or probe provider accounts. Keep both
executables, the service supervisor and plugin directory together. PATH is not
changed. A Windows policy prompt is possible for this unsigned alpha.

For managed installation and subsequent replacement/recovery, download these six
assets together: the ZIP and checksum, `usagestat-windows-x86_64.manifest.json`
and checksum, and `Install-Usagestat.ps1` and checksum. Verify the script checksum
as well, then follow [the per-user installer guide](windows-distribution.md#transactional-per-user-candidate-installer).
Use a fresh destination when switching from a manually extracted archive.
The installer supports `-BackendProfile dev` for the separate development identity;
its executable names end in `-dev.exe`.

## macOS and Linux: portable first run

Choose the archive matching your CPU. On macOS, `uname -m` reports `arm64` for
Apple Silicon or `x86_64` for Intel. Linux ARM64 uses the `aarch64` asset. Download
the archive and matching `.sha256` sidecar from the release page. For example,
on Apple Silicon:

```sh
shasum -a 256 -c usagestat-macos-aarch64.tar.gz.sha256
mkdir usagestat-v2-alpha.1
tar -xzf usagestat-macos-aarch64.tar.gz -C usagestat-v2-alpha.1
./usagestat-v2-alpha.1/usagestat --version
./usagestat-v2-alpha.1/usagestat --json list
./usagestat-v2-alpha.1/usagestat doctor
```

For Intel macOS substitute `usagestat-macos-x86_64.tar.gz`. On Linux use the
matching `usagestat-linux-x86_64.tar.gz` or `usagestat-linux-aarch64.tar.gz` and
`sha256sum -c` to verify it. Proceed with extraction only when checksum verification
succeeds, and use a fresh directory. Keep the entire extracted directory together;
invoke the CLI by its full path or add that directory to your shell's PATH.

The unsigned macOS download may be blocked by local security policy. This alpha
does not include a signed/notarized installer; do not disable Gatekeeper or strip
quarantine to treat the download as qualified. A source build from the tagged
checkout is another testing route, with the documented Rust/native build tools.

## Dashboard and providers

Run the extracted `usagestatd` (`usagestatd.exe` on Windows) with
`--bind 127.0.0.1:6737`, then open <http://127.0.0.1:6737/dashboard>. Stop foreground
execution with Ctrl+C. Choose another unused port if needed. For a first alpha
test this avoids the stable daemon's default port 6736.

To keep writable alpha settings/history separate, set `USAGESTAT_CONFIG_DIR` and
`USAGESTAT_DATA_DIR` to dedicated absolute alpha directories in the shell used
for both commands. Those overrides are full application paths. They do not move
provider-owned credentials, browser profiles or IDE data. Use a separate OS test
user for login-service acceptance if another installation already owns that
profile's service identity. A different port alone does not create a new profile.

Provider access is separate from installing the backend. Configure or sign into
the selected provider, then explicitly try `usagestat usage PROVIDER_ID`.
Check the [provider/source matrix](provider-compatibility.md) before choosing an
authentication method; available manifests are not proof of a verified account.

Login startup is optional and explicit: run the matching CLI's
`daemon enable --bind 127.0.0.1:6737`, then `daemon status`. The CLI reports an
existing owner/conflict instead of replacing another installation silently.
Use [Windows](windows-daemon.md), [macOS](macos-daemon.md) or
[shared daemon instructions](daemon-lifecycle.md) for ownership and T3 settings.

## Update or remove

Retain the previous version and a copy of any alpha state you need until a new
build has been verified. Stop and unregister an owned login daemon with that
installation's `daemon unregister` before removing its executable directory.
This retains settings, history and keys. A portable installation can then be
removed by deleting only its extraction directory. The Windows installer instead
uses its documented `Install`, `Recover` and `Uninstall` actions.

Same-build replacement, Homebrew revision relocation and interruption recovery
have native fixtures. Upgrading between distinct released backend versions is
still an alpha test case. Do not overwrite a running Windows executable, assume
an older backend understands new alpha state, or delete provider-owned credentials
as part of removing the backend.

npm remains a planned distribution channel with passing native installation
rehearsals. `@hashimkarim/usagestat` has not yet been published; use these GitHub
downloads while public npm publication is pending. Stable AUR/Homebrew/COPR/PPA
destinations are preserved alongside their separate alpha channels. Windows feed
availability is listed above; desktop stores and signed desktop bundles require
separate product/signing work.
