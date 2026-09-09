# usagestat

Track AI provider quotas, token usage, and costs through a scriptable CLI,
a local dashboard, and an HTTP API.

<p>
  <a href="https://github.com/hashimkarim/usagestat/releases/tag/v2.0.0-alpha.1"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/Preview-v2.0.0--alpha.1-B45309.svg?variant=outline&amp;size=sm&amp;logo=github&amp;mode=dark">
    <img alt="v2.0.0-alpha.1 backend preview" src="https://shieldcn.dev/badge/Preview-v2.0.0--alpha.1-B45309.svg?variant=outline&amp;size=sm&amp;logo=github&amp;mode=light">
  </picture></a>
  <a href="https://github.com/Hashim-K/usagestat/releases/latest"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/github/Hashim-K/usagestat/release.svg?variant=outline&amp;size=sm&amp;logo=github&amp;mode=dark">
    <img alt="Latest release" src="https://shieldcn.dev/github/Hashim-K/usagestat/release.svg?variant=outline&amp;size=sm&amp;logo=github&amp;mode=light">
  </picture></a>
  <a href="https://aur.archlinux.org/packages/usagestat-bin"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/AUR-package-1793D1.svg?variant=outline&amp;size=sm&amp;logo=archlinux&amp;mode=dark">
    <img alt="AUR: usagestat-bin" src="https://shieldcn.dev/badge/AUR-package-1793D1.svg?variant=outline&amp;size=sm&amp;logo=archlinux&amp;mode=light">
  </picture></a>
  <a href="https://github.com/hashimkarim/homebrew-tap/blob/main/Formula/usagestat.rb"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/Homebrew-tap-FBB040.svg?variant=outline&amp;size=sm&amp;logo=homebrew&amp;mode=dark">
    <img alt="Homebrew: hashimkarim/tap/usagestat" src="https://shieldcn.dev/badge/Homebrew-tap-FBB040.svg?variant=outline&amp;size=sm&amp;logo=homebrew&amp;mode=light">
  </picture></a>
  <a href="https://copr.fedorainfracloud.org/coprs/hashimkarim/usagestat/"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/Fedora-COPR-51A2DA.svg?variant=outline&amp;size=sm&amp;logo=fedora&amp;mode=dark">
    <img alt="Fedora COPR: hashimkarim/usagestat" src="https://shieldcn.dev/badge/Fedora-COPR-51A2DA.svg?variant=outline&amp;size=sm&amp;logo=fedora&amp;mode=light">
  </picture></a>
  <a href="https://launchpad.net/~hashimkarim/+archive/ubuntu/usagestat"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/Ubuntu-PPA-E95420.svg?variant=outline&amp;size=sm&amp;logo=ubuntu&amp;mode=dark">
    <img alt="Ubuntu PPA: hashimkarim/usagestat" src="https://shieldcn.dev/badge/Ubuntu-PPA-E95420.svg?variant=outline&amp;size=sm&amp;logo=ubuntu&amp;mode=light">
  </picture></a>
  <a href="LICENSE"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/github/Hashim-K/usagestat/license.svg?variant=outline&amp;size=sm&amp;mode=dark">
    <img alt="MIT license" src="https://shieldcn.dev/github/Hashim-K/usagestat/license.svg?variant=outline&amp;size=sm&amp;mode=light">
  </picture></a>
</p>

- Probe provider accounts through JavaScript plugins and export JSON or CSV.
- Keep usage available in a local dashboard with an optional background daemon.
- Connect to **T3 Code → Usage → Limits** through the built-in CLIProxyAPI bridge.
- Install both `usagestat` and `usagestatd`, plus bundled provider plugins, from
  your preferred package channel.

## Install

**[Try v2.0.0-alpha.1 on Windows, macOS or Linux](docs/alpha.md).** The unsigned
backend alpha includes native archives and a Windows per-user installer. Native
bar frontends, desktop acceptance, signing and public npm publication remain
pending. Stable package-manager instructions follow below.

Separate **[alpha packages and installation instructions](docs/alpha.md#alpha-package-repositories)**
are available through Fedora COPR, AUR, Homebrew (including macOS), and Scoop.
The signed Ubuntu PPA upload is being published; Chocolatey is awaiting community
review. WinGet and npm publication are still in progress. The linked guide tracks
each channel's actual availability.

<p>
  <a href="https://copr.fedorainfracloud.org/coprs/hashimkarim/usagestat-alpha/"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/Fedora-alpha-51A2DA.svg?variant=outline&amp;size=sm&amp;logo=fedora&amp;mode=dark">
    <img alt="Fedora alpha package" src="https://shieldcn.dev/badge/Fedora-alpha-51A2DA.svg?variant=outline&amp;size=sm&amp;logo=fedora&amp;mode=light">
  </picture></a>
  <a href="https://aur.archlinux.org/packages/usagestat-alpha-bin"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/AUR-alpha-1793D1.svg?variant=outline&amp;size=sm&amp;logo=archlinux&amp;mode=dark">
    <img alt="AUR alpha package" src="https://shieldcn.dev/badge/AUR-alpha-1793D1.svg?variant=outline&amp;size=sm&amp;logo=archlinux&amp;mode=light">
  </picture></a>
  <a href="https://github.com/hashimkarim/homebrew-tap/blob/main/Formula/usagestat-alpha.rb"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/Homebrew-alpha-FBB040.svg?variant=outline&amp;size=sm&amp;logo=homebrew&amp;mode=dark">
    <img alt="Homebrew alpha package" src="https://shieldcn.dev/badge/Homebrew-alpha-FBB040.svg?variant=outline&amp;size=sm&amp;logo=homebrew&amp;mode=light">
  </picture></a>
  <a href="https://github.com/hashimkarim/scoop-bucket/blob/main/bucket/usagestat-alpha.json"><picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/Scoop-alpha-3B82F6.svg?variant=outline&amp;size=sm&amp;logo=windows&amp;mode=dark">
    <img alt="Scoop alpha package" src="https://shieldcn.dev/badge/Scoop-alpha-3B82F6.svg?variant=outline&amp;size=sm&amp;logo=windows&amp;mode=light">
  </picture></a>
</p>

Choose a package manager already available on your system. Published packages
and stable release downloads target **Linux**; the stable Homebrew formula is also
Linux-only. Stable availability below was checked against release **v1.0.3**.

| Channel | Published OS / CPU support | Package |
| --- | --- | --- |
| [Arch Linux / AUR](https://aur.archlinux.org/packages/usagestat-bin) | Arch Linux · x86-64 | `usagestat-bin` — release binaries |
| [Homebrew](https://github.com/hashimkarim/homebrew-tap/blob/main/Formula/usagestat.rb) | Linux with system glibc 2.39+ · x86-64, ARM64 | `hashimkarim/tap/usagestat` |
| [Fedora COPR](https://copr.fedorainfracloud.org/coprs/hashimkarim/usagestat/) | Fedora 43, 44, 45, Rawhide · x86-64 | `usagestat` |
| [Ubuntu PPA](https://launchpad.net/~hashimkarim/+archive/ubuntu/usagestat) | Ubuntu 24.04 LTS (Noble) · amd64 | `usagestat` |
| [GitHub Releases](https://github.com/Hashim-K/usagestat/releases/latest) | Linux with glibc 2.39+ · x86-64, ARM64 | CLI + daemon + plugins in `.tar.gz` archives |

### Arch Linux

With an existing AUR helper:

```bash
yay -S usagestat-bin
```

[Helper-free installation and PKGBUILD review](docs/installation.md#arch-linux--aur).

### Homebrew on Linux

With [Homebrew installed](https://docs.brew.sh/Installation):

```bash
brew install hashimkarim/tap/usagestat
```

The release binaries require **system glibc 2.39+** (for example, Ubuntu 24.04).
For older systems, use a [source build](docs/installation.md#from-source).

If Homebrew asks for trust, review and approve this formula. See the
[formula-scoped trust instructions](docs/installation.md#homebrew-on-linux).

### Fedora

```bash
sudo dnf install dnf5-plugins
sudo dnf copr enable hashimkarim/usagestat
sudo dnf install usagestat
```

### Ubuntu 24.04

```bash
sudo apt update
sudo apt install software-properties-common
sudo add-apt-repository ppa:hashimkarim/usagestat
sudo apt update
sudo apt install usagestat
```

For direct downloads, source builds, upgrades, and removal, see the
[installation guide](docs/installation.md).

## First run

```bash
usagestat --version
usagestat list
usagestat config validate
```

You should see the installed version, discovered providers, and configuration
validation results. Provider access is separate from installation: sign into
its supported CLI/app or configure its credentials before probing. For example:

```bash
usagestat usage claude
usagestat --json usage claude
```

To include a provider in regular polling, add it to
`~/.config/usagestat/config.toml` (or the corresponding XDG config directory):

```toml
[[providers]]
id = "claude"
enabled = true
```

On Linux with a systemd user session, start the dashboard and enable startup at
login:

```bash
usagestat daemon enable
usagestat daemon status
usagestat dashboard
```

The dashboard is at <http://127.0.0.1:6736/dashboard>. Use
`usagestat dashboard --url` to print its link over SSH. Without systemd, run
`usagestatd` in a terminal and open that URL while it is running. CLI probes work
without starting the daemon.

### Connect T3 Code

```bash
usagestat daemon t3 auto
usagestat daemon key
```

With the daemon running, add `http://127.0.0.1:6736` as a CLIProxyAPI hub in T3
Code and paste the printed management key. T3 mode is remembered across daemon
stops and restarts. `usagestat daemon t3 off` disables the bridge;
`usagestat daemon disable` stops the daemon and turns off autostart.
See the [T3 Code setup guide](docs/t3-code.md) for supported quotas and details.

## Providers and configuration

Plugins are discovered from:

1. `USAGESTAT_PLUGIN_DIR`
2. `~/.config/usagestat/plugins`
3. Installed `share/usagestat/plugins` and `lib/usagestat/plugins` under the binary prefix
4. `./plugins`

Bundled providers include:

- `abacus-ai`, `alibaba`, `alibaba-token-plan`, `amp`, `antigravity`, `augment`
- `aws-bedrock`, `azure-openai`, `claude`, `codebuff`, `codex`, `command-code`
- `copilot`, `crof`, `cursor`, `deepgram`, `deepseek`, `doubao`, `droid`
- `elevenlabs`, `factory`, `gemini`, `grok`, `groqcloud`, `jetbrains-ai-assistant`
- `kilo`, `kimi`, `kimi-k2`, `kiro`, `llm-proxy`, `manus`, `mimo`, `minimax`
- `mistral`, `moonshot`, `nanogpt`, `ollama`, `openai-api`, `opencode`
- `opencode-go`, `openrouter`, `perplexity`, `stepfun`, `synthetic`, `t3chat`
- `venice`, `vertex-ai`, `warp`, `windsurf`, `zai`

`usagestat --json list` includes provider-owned UI metadata. Icon paths are
resolved to absolute SVG paths; `icon.path` is the monochrome/default icon and
`icon.colorPath` is present only when a separate color SVG is available.

<details>
<summary>Configuration paths and advanced example</summary>

Default config path:

```text
~/.config/usagestat/config.toml
```

Default cache path:

```text
~/.local/share/usagestat/snapshots.json
```

An advanced example with separate accounts and sources (`mock` requires the
dev-only plugins described below):

```toml
refreshSec = 60
pluginDirs = ["/path/to/more/plugins"]

[[providers]]
id = "mock"
enabled = true

[[providers]]
id = "claude"
instanceId = "claude-web"
displayName = "Claude Web"
enabled = true
source = "web"
cookieHeader = "sessionKey=..."

[[providers]]
id = "openai-api"
instanceId = "openai-api-eu"
displayName = "OpenAI API EU"
enabled = true
source = "api"
apiKey = "sk-..."
region = "eu"
workspaceId = "workspace-123"

[[providers]]
id = "custom"
instanceId = "local-script"
displayName = "Local Usage Script"
enabled = true
source = "custom"
customCommand = "/path/to/usage-script --json"
```

Both binaries accept overrides:

```bash
cargo run -p usagestat-cli -- --config ./config.toml --plugin-dir ./plugins list
cargo run -p usagestat-daemon -- --config ./config.toml --refresh-sec 30
```

</details>

## Source checkout features

The dedicated **History** tab is available in the source/dev build and is pending
the next package release. History adds provider/date filters, daily/weekly/monthly
charts, previous-period comparisons, and CSV export of saved daily reports.
Quota-only providers retain their snapshot charts. See
[CLI and dashboard documentation](docs/cli.md).

## Development

Build and install a separate dev profile with a current stable Rust toolchain
and the [source-build prerequisites](docs/installation.md#from-source):

```bash
scripts/install-dev.sh
usagestat-dev --version
```

`usagestat-dev daemon enable` uses the separate `usagestat-dev.service` and dev
config/data directories.

This starts from CrossUsage's architecture, but uses separate project names and
contracts:

- `usagestat-core`: shared models, config, paths, cache.
- `usagestat-plugins`: JavaScript provider plugin loader/runtime.
- `usagestat-cli`: scriptable CLI.
- `usagestat-daemon`: local polling daemon with an HTTP API.

<details>
<summary>Run commands directly from the checkout</summary>

```bash
cargo run -p usagestat-cli -- list
cargo run -p usagestat-cli -- --plugin-dir templates/dev-providers --json usage mock
cargo run -p usagestat-cli -- usage --provider claude --save
cargo run -p usagestat-cli -- status claude codex
cargo run -p usagestat-cli -- export --format csv
cargo run -p usagestat-cli -- auth import-cookies --provider codex --format json
cargo run -p usagestat-cli -- config validate
cargo run -p usagestat-cli -- cache clear --history
cargo run -p usagestat-cli -- plugin validate
cargo run -p usagestat-daemon
curl http://127.0.0.1:6736/v1/usage
```

</details>

## HTTP API

Endpoints in the source checkout:

- `GET /health`
- `GET /v1/providers`
- `GET /v1/usage`
- `GET /v1/usage/:providerId`
- `GET /v0/management/quota-scheduler/status` (opt-in, management key required)

See [CLI documentation](docs/cli.md) for usage/history exports and daemon controls.

<details>
<summary>Plugin host API</summary>

Provider plugins export `globalThis.__usagestat_plugin.probe(ctx)`.

A copyable plugin template lives at `templates/provider-plugin`. It includes
examples for `api`, `oauth`, `local`, `cli`, and `web` source modes. Dev-only
example providers live under `templates/dev-providers` and are not loaded or
packaged as production providers.

Available context:

- `ctx.nowIso`
- `ctx.sourceMode`
- `ctx.app.version`
- `ctx.app.platform`
- `ctx.app.appDataDir`
- `ctx.app.pluginDataDir`
- `ctx.host.log.info|warn|error(message)`
- `ctx.host.env.get(name)` for allowlisted variables
- `ctx.host.fs.homeDir`
- `ctx.host.fs.exists(path)`
- `ctx.host.fs.readText(path)`
- `ctx.host.fs.listDir(path)`
- `ctx.host.http.request({ url, method, headers, bodyText, timeoutMs })`
- `ctx.host.command.run({ program, args, timeoutMs })` for allowlisted commands

Host HTTP responses use:

```json
{
  "status": 200,
  "headers": {},
  "bodyText": "{}"
}
```

</details>

## Documentation and contributing

- [Installation, upgrades, and removal](docs/installation.md)
- [CLI reference and troubleshooting](docs/cli.md)
- [T3 Code integration](docs/t3-code.md)
- [GNOME extension integration](docs/gnome-extension.md)
- [Release publishing and platform maintenance](docs/distribution.md)
- [Provider plugin template](templates/provider-plugin/README.md)
- [Changelog](CHANGELOG.md)

For changes, run `cargo test --locked --workspace`. Report bugs or propose
changes through [GitHub issues](https://github.com/Hashim-K/usagestat/issues).

## License

[MIT](LICENSE). This project builds on CrossUsage's architecture.
