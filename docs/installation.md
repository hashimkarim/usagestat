# Installation, upgrades, and removal

Install the CLI (`usagestat`), daemon (`usagestatd`), and provider plugins together.
Use the [published support table](../README.md#install) to choose a channel for
your system. The package channels below currently serve **v1.0.3**. For work
from a source checkout, see [development](../README.md#development).

For the Windows/macOS/Linux **v2.0.0-alpha.1** backend downloads, use the separate
[alpha installation guide](alpha.md). Alpha archives are unsigned and public npm
publication remains pending; the package-manager commands below install stable.

The native macOS Homebrew adapter and installation rehearsal are tracked in
[macOS distribution](macos-distribution.md). Stable macOS packages and signing
remain pending; the commands for published Linux packages below retain their scope.
Windows candidate ZIP and native development instructions are in
[Windows distribution](windows-distribution.md).

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

## Homebrew on Linux

The [tap formula](https://github.com/hashimkarim/homebrew-tap/blob/main/Formula/usagestat.rb)
supports Linux x86-64 and ARM64 with **system glibc 2.39 or newer**. It does not
provide macOS binaries. Homebrew itself can run on older Linux systems, but its
bundled glibc does not make this formula's prebuilt binaries compatible with
them. Check your system version with `getconf GNU_LIBC_VERSION`; use a
[source build](#from-source) on older systems. After
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
has v1.0.3 builds for **Fedora 43, 44, 45, and Rawhide, on x86-64**.
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
publishes **Ubuntu 24.04 LTS (Noble), amd64**, with package version
`1.0.3-1ppa2`. The suffix is a Debian packaging revision; the program reports
`1.0.3`. Other Ubuntu series and ARM64 do not currently have published packages
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

Check glibc with `getconf GNU_LIBC_VERSION`. Both v1.0.3 architectures require
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
git clone --branch v1.0.3 --depth 1 https://github.com/Hashim-K/usagestat.git
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
`cargo install --path crates/ai-usage-cli` alone installs only the CLI and omits
the daemon and bundled plugin resources.

## Verify and start

```bash
usagestat --version
usagestat list
usagestat config validate
```

The packaged version should report `usagestat 1.0.3`, `list` should find the
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

If the managed daemon is running, run `usagestat daemon enable` after upgrading
to restart it with the updated binary. This preserves its saved T3 mode.
For a foreground daemon, stop it and launch `usagestatd` again. Then check
`usagestat --version` and `usagestat daemon status`.

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
