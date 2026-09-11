# Native provider paths

Implementation and qualification work for #11. These changes preserve the
existing usage parsers, aggregation, reset calculations and pricing tables.

Claude's daemon collector reads `CLAUDE_CONFIG_DIR/projects` when configured.
Otherwise it uses the native home directory and `.claude/projects`; the Xcode
Claude history location is included only on macOS without an explicit profile.
An explicit profile excludes both defaults, including when it contains no data.

Codex's daemon collector reads `sessions` and `archived_sessions` in `CODEX_HOME`
or the native home's `.codex`. An explicit root must be an existing accessible
directory and is canonicalized, matching upstream resolution. A missing or
invalid explicit root returns `LOCAL_USAGE_PATH_UNAVAILABLE` from the local usage
endpoint; it does not fall back to another profile or a saved report.

Cursor stable and nightly use separate native application directories and
separate `CURSOR_STATE_DB` / `CURSOR_NIGHTLY_STATE_DB` overrides. Nightly does not
consume the stable override. An invalid explicit database does not fall back.
The shared IDE app-support resolver uses Windows APPDATA (roaming Known Folders
when APPDATA is unset), macOS
Application Support or Linux XDG config. It does not search copied trees from
another OS or the old Linux config root after XDG redirection.

Kiro now resolves its existing SQLite, log and profile layouts beneath native
app support, with `settings.userDataDir` for an explicit IDE data root.
Windsurf/Devin resolves four separate folders: `Windsurf`, `Windsurf - Next`,
`Devin` and `Devin - Next`. If more than one contains auth, it requires
`settings.ideVariant` (`windsurf`, `windsurf-next`, `devin-windsurf`, or
`devin-next-windsurf`) before issuing a request. A custom `settings.userDataDir`
requires that variant too. An unsuccessful selected account never switches to
another installation.

JetBrains uses native app support and supports `settings.configDir`, the complete
IDE configuration directory corresponding to upstream `idea.config.path`.
Multiple discovered quota profiles require that selection instead of choosing
whichever quota looks newest or most used. These changes port the existing
provider-specific file layouts; they do not establish compatibility with new
upstream schema versions, remote IDE profiles, portable installs or undocumented
auth stores. The Node harness tests all three OS path conventions with synthetic
redirected roots and reuses the native host's actual JavaScript utility code.

The native root behavior follows
[VS Code's user-data resolver](https://github.com/microsoft/vscode/blob/main/src/vs/platform/environment/node/userDataPath.ts).
The carried Kiro suffixes and preview-variant names still require real-app version
qualification. Devin documents the stable IDE roots below; private auth schemas
remain unverified.
[JetBrains documents native directories and `idea.config.path`](https://www.jetbrains.com/help/idea/directories-used-by-the-ide-to-store-settings-caches-plugins-and-logs.html).

Cursor plugin authentication also uses the native database resolver. A missing
explicit database cannot fall through to another app's paths. Database auth is
authoritative when present; generic CLI keychain tokens cannot replace it based
on a different account's plan. Shared CLI credentials/transcript activity are
excluded for Nightly and explicitly selected database profiles.

Claude uses its profile credential file on Linux/Windows and the current user's
Keychain account on macOS, with the same profile's file as the documented
fallback. An explicit `CLAUDE_CONFIG_DIR` selects only its NFC-hashed service;
the host now supplies the SHA-256 helper that this lookup requires. It does not
fall through to the default profile or broaden the account lookup. If no valid
file fallback exists, denied/locked store errors remain distinct from missing
auth. This follows [Claude's credential storage documentation](https://code.claude.com/docs/en/authentication).

Enabling startup at login saves explicit `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`CURSOR_STATE_DB`, `CURSOR_NIGHTLY_STATE_DB` and `CURSOR_AGENT_HOME` roots in the
private service settings. Roots must be absolute so login cannot change their
meaning or a raw-path credential hash. Unset variables retain existing service
settings; an explicitly empty value clears that override. No provider API tokens
are captured in this environment list.

Plugin SQLite reads open the real database read-only, with a 250 ms busy timeout
and connection-level query-only mode. They respect active WAL writers and
committed snapshots. The old immutable fallback is removed because its
assumptions do not hold for a running IDE. A locked or inaccessible database
reports a read failure; it is never created or opened writable.

`tools/portability/local_usage.py` runs synthetic histories through a disposable
daemon and checks the daily/weekly/monthly/session reports, account isolation,
Unicode paths, archived Codex history and malformed/empty lines. Windows runs
with no HOME environment variable. `provider_storage.py` queries a synthetic
database while a writer holds WAL and exclusive transactions, rejects mutation
and checks bounded lock recovery. Both fixtures run in the five-target native
CI gate. Real provider credentials, app versions and running-app qualification
remain separate release gates; no account is marked verified by these fixtures.

Resolution references checked 2026-09-07:
[Claude directory and override](https://code.claude.com/docs/en/claude-directory),
[Codex native home resolver](https://github.com/openai/codex/blob/main/codex-rs/utils/home-dir/src/lib.rs),
[SQLite immutable URI semantics](https://www.sqlite.org/uri.html#uriimmutable).

Antigravity's combined provider now searches only native `Antigravity` and
`Antigravity IDE` roots. `settings.ideVariant` (`antigravity` or
`antigravity-ide`) or `settings.userDataDir` selects a database without trying
another process, CLI store or profile. Multiple discovered databases with auth
require selection. Refreshed-token cache entries include a hash of the selected
database path and its refresh-token identity; old unscoped entries are ignored.
Native process ambiguity/denial remains explicit. The separate CLI provider
reads only the exact `gemini` / `antigravity` account, with no service-only retry.
These carried credential formats still need qualification with current app
versions; root portability does not verify a private schema.

The Devin provider supports `settings.authSource` (`auto`, `cli`, `ide`, `manual`),
`settings.credentialsPath`, and `settings.ideVariant` (`devin`, `devin-next`,
`windsurf`, `windsurf-next`). A custom `settings.userDataDir` requires an IDE
variant. Different accounts found in CLI/IDE installations require selection;
auth failures never try another account. Identical credentials in migrated
installations are deduplicated. The CLI reader keeps the existing Unix
`.local/share/devin` then legacy `cognition` layout. On Windows it uses native
local app data, honoring absolute `LOCALAPPDATA` and otherwise Known Folders,
without guessed home-relative AppData. That CLI file location/schema remains
unverified for current versions; `credentialsPath` selects the actual file.
[Devin's FAQ documents its native IDE roots and migration](https://docs.devin.ai/desktop/devin-desktop-faq).

Devin web quotas also accept an explicit bearer token and organization on Linux,
macOS and Windows:

```toml
[[providers]]
id = "devin"
enabled = true
source = "web"
workspace_id = "org_YOUR_ORGANIZATION"
cookie_header = "Bearer YOUR_DEVIN_WEB_TOKEN"
```

`settings.authSource = "manual"` selects this path in auto mode too. The
organization can be an internal ID, slug, or `https://app.devin.ai/org/...` URL.
`DEVIN_BEARER_TOKEN` then `DEVIN_AUTHORIZATION` override the configured token;
`DEVIN_ORGANIZATION` then `DEVIN_ORG` override the configured organization.
An explicitly empty override stops authentication instead of selecting the
configured value. Environment credentials alone do not select web authentication;
choose `source = "web"` or manual auth explicitly. `settings.cookieSource = "off"`
disables this path. Use CLI/IDE selection separately from manual web credentials.
Web token rejection stops the request without trying another account. These
web tokens are distinct from the existing CLI/IDE Codeium credentials.

OpenCode Go uses `$XDG_DATA_HOME/opencode`, defaulting to the native home's
`.local/share/opencode`, on **all three OSes**. `settings.dataDir` selects the
complete OpenCode data directory. `settings.databasePath` selects an absolute
database; otherwise `OPENCODE_DB` selects an absolute database or a filename
relative to that data directory. In-memory history is explicitly unsupported.
The default database is `opencode.db`; preview/channel-specific databases require
an explicit selection. `OPENCODE_CONFIG_DIR` does not move auth/history, and the
test-only `OPENCODE_TEST_HOME` does not move upstream data. Nonempty
`OPENCODE_AUTH_CONTENT` supplies the selected auth object without reading another
file; malformed inline auth fails explicitly. These credentials are never saved
in the daemon's captured environment. Startup setup retains `XDG_DATA_HOME` and
`OPENCODE_DB`; explicit data roots must be absolute.
The existing usage calculations and web enrichment remain unchanged; current
database schemas and live accounts still need qualification.
This follows OpenCode's
[global roots](https://github.com/anomalyco/opencode/blob/53fec37d8d2b9e0d92a1b4184e8df8f8480a2d26/packages/core/src/global.ts),
[database selection](https://github.com/anomalyco/opencode/blob/53fec37d8d2b9e0d92a1b4184e8df8f8480a2d26/packages/core/src/database/database.ts),
[auth selection](https://github.com/anomalyco/opencode/blob/53fec37d8d2b9e0d92a1b4184e8df8f8480a2d26/packages/opencode/src/auth/index.ts),
and [xdg-basedir](https://github.com/sindresorhus/xdg-basedir/blob/main/index.js).

Zed settings use Roaming AppData `Zed/settings.json` on Windows, XDG config
`zed/settings.json` on Linux (with `FLATPAK_XDG_CONFIG_HOME` taking precedence),
and the native home's `.config/zed/settings.json` on macOS. This distinction
between settings and application data follows
[Zed's resolver](https://github.com/zed-industries/zed/blob/1870e269ad88802147f2baec3086abb67d17260a/crates/paths/src/paths.rs).
`settings.settingsPath` selects one absolute file; `settings.userDataDir` matches
Zed's custom data directory and selects its `config/settings.json`. Comments,
trailing commas and a UTF-8 BOM are supported. Missing explicit settings or
unreadable/malformed settings report an error before any credential or API
request, preserving custom-server identity. These path fixtures do not verify
the private OS credential representation used by a particular Zed release.
Startup setup retains an explicit absolute `FLATPAK_XDG_CONFIG_HOME`.

Perplexity's existing app-cache reader requires the legacy macOS CFNetwork
SQLite schema and Apple app request metadata. Speculative Chromium/Linux/Windows
cache paths have been removed. This authentication method reports `unsupported`
on Linux/Windows before looking for files; it does not establish that the
provider or upstream desktop app is unsupported. On macOS `settings.cacheDbPath`
selects one exact legacy cache. This plugin currently has no manual web-cookie
implementation despite its manifest declaring a web mode; that is an explicit
inventory gap, not a supported fallback. Current app versions also need schema
qualification; [Perplexity documents an app migration](https://www.perplexity.ai/help-center/en/collections/19800000-perplexity-desktop-app).
