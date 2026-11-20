# Installation, upgrades, and removal

Install the CLI (`usagestat`), daemon (`usagestatd`), and provider plugins together.
Use the [published support table](../README.md#install) to choose a channel for
your system. The current stable release is **[v2.4.0](releases/v2.4.0.md)**; its
[publication record](releases/v2.4.0-publication.md) tracks package-feed
availability. Do not assume a queued feed build is already installable. For work
from a source checkout, see [development](../README.md#development).

Stable releases cover Linux x64/ARM64 (glibc 2.39+), macOS 11+ on Apple Silicon
and Intel, and Windows 10 / Server 2016+ on x64. macOS and Windows builds are
**unsigned**. Release CI installs and runs them on macOS 15 (both CPUs) and
Windows Server 2025; older versions within those floors are untested. Desktop
bar frontends for macOS and Windows are not included. Platform background is in
[macOS distribution](macos-distribution.md) and
[Windows distribution](windows-distribution.md).

## npm

With Node 24+ and npm 11.5.1+ on Linux, macOS or Windows:

```sh
npm install --global @hashimkarim/usagestat@latest --include=optional --ignore-scripts
```

npm selects the exact-version native package for your OS and CPU; Node 24 itself
needs macOS 13.5+. Installation does not configure a profile or start a service.
See [npm lifecycle details](../npm/README.md).

## Arch Linux / AUR

[`usagestat-bin`](https://aur.archlinux.org/packages/usagestat-bin) installs the
published Linux x86-64 binaries. With an existing AUR helper:

```bash
yay -S usagestat-bin
```

Without a helper, install the [AUR prerequisites](https://wiki.archlinux.org/title/Arch_User_Repository),
then clone and review the recipe:

```bash
sudo pacman -Syu --needed base-devel git
git clone https://aur.archlinux.org/usagestat-bin.git
cd usagestat-bin
less PKGBUILD .SRCINFO
makepkg -si
```

Review accompanying build/install files too. Run `makepkg` as your regular user.
The package installs both binaries in `/usr/bin` and the bundled plugins in
`/usr/share/usagestat/plugins`.

## Homebrew

The [tap formula](https://github.com/hashimkarim/homebrew-tap/blob/main/Formula/usagestat.rb)
supports macOS 11+ (Apple Silicon and Intel) and Linux x86-64 and ARM64 with
**system glibc 2.39 or newer**. On Linux, Homebrew's bundled glibc does not make
the prebuilt binaries compatible with older systems; check with
`getconf GNU_LIBC_VERSION` and use a [source build](#from-source) if needed. After
[installing Homebrew](https://docs.brew.sh/Installation):

```bash
brew install hashimkarim/tap/usagestat
```

The fully qualified formula automatically selects the correct tap. On Homebrew
versions that ask for trust, review the linked formula and approve it. For
noninteractive use on versions with [`brew trust`](https://docs.brew.sh/Manpage#trust-options-target-):

```bash
brew trust --formula hashimkarim/tap/usagestat
brew install hashimkarim/tap/usagestat
```

This grants trust to this formula. Its binaries and plugins are installed under
Homebrew's prefix and discovered automatically.

## Fedora / COPR

For the v2 preview, use the separate [alpha COPR instructions](alpha.md#alpha-package-repositories).

The [COPR project](https://copr.fedorainfracloud.org/coprs/hashimkarim/usagestat/)
targets **Fedora 43, 44, 45, and Rawhide, on x86-64**.
These targets use DNF5; its COPR command is supplied by
[`dnf5-plugins`](https://packages.fedoraproject.org/pkgs/dnf5/dnf5-plugins/).

```bash
sudo dnf install dnf5-plugins
sudo dnf copr enable hashimkarim/usagestat
sudo dnf install usagestat
```

The package includes both binaries and `/usr/share/usagestat/plugins`.

## Ubuntu / PPA

The [PPA](https://launchpad.net/~hashimkarim/+archive/ubuntu/usagestat) currently
publishes **Ubuntu 24.04 LTS (Noble), amd64**. Packages are versioned like
`2.4.0-1ppa1`; the suffix is a Debian packaging revision; the program reports the upstream
version. See the [v2.4.0 publication record](releases/v2.4.0-publication.md) for
publication and verification evidence.
Other Ubuntu series and ARM64 do not currently have published packages
in this PPA. These instructions are for Ubuntu, not Debian.

```bash
sudo apt update
sudo apt install software-properties-common
sudo add-apt-repository ppa:hashimkarim/usagestat
sudo apt update
sudo apt install usagestat
```

The package includes both binaries and `/usr/share/usagestat/plugins`.
Launchpad also documents [how PPA repositories work](https://launchpad.net/+help-soyuz/ppa-sources-list.html).

## Direct release downloads

[GitHub Releases](https://github.com/Hashim-K/usagestat/releases/latest) provides
these archives, each with a corresponding `.sha256` file:

| Linux CPU | Archive |
| --- | --- |
| x86-64 / amd64 | `usagestat-linux-x86_64.tar.gz` |
| ARM64 / aarch64 | `usagestat-linux-aarch64.tar.gz` |

Use a Linux system with **glibc 2.39+**, `curl`, `tar`, and coreutils (`sha256sum`,
`install`). The archive contains both binaries, plugins, and the license;
standalone `usagestat-linux-*` downloads contain only the CLI.

Check glibc with `getconf GNU_LIBC_VERSION`. Both Linux architectures require
2.39; for an older host, build from source instead of using these archives.

Run this in an empty working directory. It downloads the latest stable release,
checks its checksum, and installs under your user's `~/.local` prefix:

```bash
case "$(uname -m)" in
  x86_64) usagestat_arch=x86_64 ;;
  aarch64|arm64) usagestat_arch=aarch64 ;;
  *) echo "No release binary for this CPU" >&2; exit 1 ;;
esac
usagestat_asset="usagestat-linux-${usagestat_arch}.tar.gz"
usagestat_download="https://github.com/Hashim-K/usagestat/releases/latest/download"
curl -fLO "${usagestat_download}/${usagestat_asset}"
curl -fLO "${usagestat_download}/${usagestat_asset}.sha256"
sha256sum --check "${usagestat_asset}.sha256" &&
tar -xzf "${usagestat_asset}" &&
install -d "$HOME/.local/bin" "$HOME/.local/lib/usagestat" &&
install -m 0755 usagestat usagestatd "$HOME/.local/bin/" &&
cp -a plugins "$HOME/.local/lib/usagestat/"
```

Ensure `~/.local/bin` is on PATH. For the current shell:

```bash
export PATH="$HOME/.local/bin:$PATH"
usagestat --version
usagestat list
```

Add that PATH setting to your shell's startup file if needed. Keep both binaries
under the same prefix as `lib/usagestat/plugins` so plugin discovery works
when you leave the download directory. Avoid mixing this manual installation
with a package-manager installation of the same commands.

### macOS archive

Choose `usagestat-macos-aarch64.tar.gz` for Apple Silicon (`uname -m` reports
`arm64`) or `usagestat-macos-x86_64.tar.gz` for Intel, with its `.sha256` file:

```sh
usagestat_asset=usagestat-macos-aarch64.tar.gz
usagestat_download="https://github.com/Hashim-K/usagestat/releases/latest/download"
curl -fLO "${usagestat_download}/${usagestat_asset}"
curl -fLO "${usagestat_download}/${usagestat_asset}.sha256"
shasum -a 256 -c "${usagestat_asset}.sha256" &&
mkdir usagestat && tar -xzf "${usagestat_asset}" -C usagestat &&
./usagestat/usagestat --version
```

Keep the extracted directory together and add it to PATH, or install with
Homebrew instead. The binaries are unsigned: a browser download can be blocked by
Gatekeeper, while `curl` downloads and Homebrew are not quarantined. Do not
disable Gatekeeper globally to run them.

### Windows

Download these assets from the release into one folder:
`usagestat-windows-x86_64.zip`, `usagestat-windows-x86_64.manifest.json` and
`Install-Usagestat.ps1`, each with its `.sha256` file. Check the hashes with
`Get-FileHash`, then install per user without elevation:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-Usagestat.ps1 -Manifest .\usagestat-windows-x86_64.manifest.json
& (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\usagestat\bin\usagestat.exe') --version
```

The execution-policy option applies to that one command. The binaries are
unsigned, so SmartScreen or a local policy may warn. The installer verifies every
checksum, installs to `%LOCALAPPDATA%\Programs\usagestat`, leaves PATH unchanged
and does not enable a login task. To upgrade, stop the daemon, run the same
command with the new release's files, and start it again. Details and recovery
are in [Windows distribution](windows-distribution.md#transactional-per-user-candidate-installer).

## From source

Use a current stable [Rust toolchain](https://www.rust-lang.org/tools/install),
Git, a C compiler, `make`, and `pkg-config` on Linux. For example, the non-Rust
prerequisites on Ubuntu are:

```bash
sudo apt update
sudo apt install build-essential pkg-config git
```

Build the current stable tag and install both binaries plus plugins:

```bash
git clone --branch v2.4.0 --depth 1 https://github.com/Hashim-K/usagestat.git
cd usagestat
cargo build --release --locked -p usagestat-cli -p usagestat-daemon
install -d "$HOME/.local/bin" "$HOME/.local/lib/usagestat"
install -m 0755 target/release/usagestat target/release/usagestatd "$HOME/.local/bin/"
cp -a plugins "$HOME/.local/lib/usagestat/"
export PATH="$HOME/.local/bin:$PATH"
```

For development from the current checkout, use the separate
[dev installer](../README.md#development). Its commands are named
`usagestat-dev` and `usagestatd-dev`, with a separate config/data profile.
`cargo install --path crates/usagestat-cli` alone installs only the CLI and omits
the daemon and bundled plugin resources.

## Verify and start

```bash
usagestat --version
usagestat list
usagestat config validate
```

A v2.4.0 installation should report `usagestat 2.4.0`, `list` should find the
bundled providers, and config validation should succeed with an absent/default
config or a valid custom config. Provider login is separate: use the
[first-run setup](../README.md#first-run) before requesting live account usage.

The daemon is optional. On Linux with a systemd user session:

```bash
usagestat daemon enable
usagestat daemon status
usagestat dashboard
```

`daemon enable` starts the service and enables startup at login. The dashboard
opens at <http://127.0.0.1:6736/dashboard>. For a terminal-only session,
`usagestat dashboard --url` prints the link. Without systemd, run `usagestatd`
in the foreground and open the URL while it is running.

If providers are missing, check `command -v usagestat` for an old binary
shadowing the package, then check the installed plugin directory. Configuration
and provider setup details are in the [CLI reference](cli.md).

## Upgrade

| Channel | Commands |
| --- | --- |
| AUR helper | `yay -Syu` |
| AUR without a helper | In the recipe checkout, run `git pull --ff-only`, review the updated `PKGBUILD` and accompanying files, then `makepkg -si`. |
| Homebrew | `brew update`, then `brew upgrade hashimkarim/tap/usagestat` |
| Fedora COPR | `sudo dnf upgrade usagestat` |
| Ubuntu PPA | `sudo apt update`, then `sudo apt install --only-upgrade usagestat` |
| Manual / source | Repeat the download or build/install steps for the new release, replacing both binaries and refreshing bundled plugins. |

A running managed daemon follows upgrades by itself. Once a minute it checks
its installed executable; when a package manager has replaced it and the new
file has stayed unchanged for 30 seconds and reports a `usagestatd` version, the
daemon shuts down normally, saving its state, and restarts in
place into the new version. It keeps its process, settings, T3 mode and
autostart. This works for RPM, deb, AUR, npm and manual installs that replace
the file at the same path, and needs no package scriptlet. A stopped daemon
stays stopped, and foreground or development runs are not watched.

These cases still need a manual restart (`usagestat daemon stop`, then
`usagestat daemon start`):

- Upgrading *to* the first version with this behavior, because the daemon that
  is running when you upgrade does not have it yet.
- Homebrew, which installs each version under a new `Cellar` path. Run
  `usagestat daemon relocate` after `brew upgrade` (see the [CLI reference](cli.md)).
- Windows, which cannot replace a running executable. Stop the daemon before
  upgrading and start it afterwards.

Check the result with `usagestat --version` and `usagestat daemon status`.

### Automatic updates on Fedora

Stable RPMs include the `hashimkarim/usagestat` COPR feed configuration with RPM
signature checks enabled, as `%config(noreplace)`, preserving administrator edits. Alpha packages do not
install the stable feed or its download timer. For optional daily downloads,
use the timer shipped by the stable RPM:

```bash
sudo systemctl enable --now usagestat-rpm-update.timer
```

The timer checks daily, with up to 30 minutes of random delay and a catch-up
after the machine was off. DNF5 upgrades `usagestat` and required dependencies,
selects Usagestat from the stable feed, and retains RPM signature checks. It
does not enroll the rest of the OS in automatic updates. Package installation
follows the system's timer preset policy.

Check the timer with:

```bash
systemctl list-timers usagestat-rpm-update.timer
journalctl -u usagestat-rpm-update.service
```

To disable daily downloads, run `sudo systemctl disable --now
usagestat-rpm-update.timer`. Manual upgrades keep working, and the running
daemon still restarts into them.

## Remove

Before uninstalling, stop the managed daemon and turn off autostart:

```bash
usagestat daemon disable
```

For a manually started daemon, stop its foreground process instead.

| Channel | Remove the package |
| --- | --- |
| AUR | `sudo pacman -R usagestat-bin` |
| Homebrew | `brew uninstall hashimkarim/tap/usagestat` |
| Fedora COPR | `sudo dnf remove usagestat` |
| Ubuntu PPA | `sudo apt remove usagestat` |

For the manual/source installation above, remove only the installed binaries and
bundled resources:

```bash
rm "$HOME/.local/bin/usagestat" "$HOME/.local/bin/usagestatd"
rm -r "$HOME/.local/lib/usagestat/plugins"
```

Configuration, management keys, and saved usage remain in the config/data
directories. In particular, `~/.local/share/usagestat/plugins` contains provider
state and should be retained. The manual install above puts bundled plugin code
under `~/.local/lib/usagestat/plugins` so it can be removed separately.

### Optional repository removal

Keep a shared tap or repository if you use it for other packages. After removing
its last package, these optional commands remove the channel configuration:

```bash
brew untap hashimkarim/tap
sudo dnf copr remove hashimkarim/usagestat
sudo add-apt-repository --remove ppa:hashimkarim/usagestat
sudo apt update
```

Use only the commands for the channel you installed. AUR does not add a pacman
repository; its downloaded recipe checkout can be removed separately.
