# Native browser authentication

Implementation for #18 and #19. Browser import and manual provider credentials
are separate methods. Native fixtures do not establish a working session with a
real provider or current browser version; that qualification remains in #20.

[Native run 34086413974](https://github.com/hashimkarim/usagestat/actions/runs/34086413974)
at `a3ee0f9` passed all five supported build targets. Windows fixtures use actual
DPAPI and authenticated synthetic AES-GCM data. Both Mac fixtures use an owned
temporary Keychain, deleted after exact service/account reads; the login Keychain
is not unlocked or replaced. The CLI fixture exercises a live SQLite writer,
scoped import, explicit unsupported formats, locks and retained manual credentials.

`auth import-cookies` reads the signed-in user's Chromium-family profile and
returns the existing JSON contract (`providerId`, `cookieHeader`, `source`,
`profile`). Import does not save, replace or clear provider configuration. The
JSON output contains a session credential and is intended for the calling client;
normal text output reports the selected browser/profile without printing it.

```sh
usagestat auth import-cookies --provider t3chat --browser chrome --profile 'Profile 1' --format json
usagestat auth curl --provider t3chat --format json
```

`--browser` accepts `chrome`, `brave`, or `chromium`. `--profile` is a single
profile directory name, including spaces/Unicode. `--user-data-dir` selects an
absolute browser data root and requires `--browser`; it never falls back to a
different installation. Without selection, multiple profiles containing cookies
for the provider URL produce `AMBIGUOUS_PROFILE` before any Keychain lookup.
No Firefox, Safari, Edge, browser extension or remote-profile support is claimed.

| Browser | Linux config root suffix | macOS Application Support suffix | Windows local app-data suffix |
| --- | --- | --- | --- |
| Chrome | `google-chrome` | `Google/Chrome` | `Google/Chrome/User Data` |
| Brave | `BraveSoftware/Brave-Browser` | `BraveSoftware/Brave-Browser` | `BraveSoftware/Brave-Browser/User Data` |
| Chromium | `chromium` | `Chromium` | `Chromium/User Data` |

Linux uses XDG config; macOS uses native Application Support. Windows honors an
absolute `LOCALAPPDATA`, with Known Folder fallback, and requires no Unix HOME.
Profiles include `Default`, `Profile *`, or an explicitly selected directory.
Within one profile, `Network/Cookies` takes precedence over legacy `Cookies`.
These roots follow [Chromium's user-data documentation](https://chromium.googlesource.com/chromium/src/+/HEAD/docs/user_data_dir.md)
and the existing Brave product layout; preview/custom installations should select
their exact data root and remain unqualified until tested.

| Platform/method | Implemented behavior | Remaining qualification |
| --- | --- | --- |
| All: plaintext `value` | Exact scoped cookie bytes | Real current-browser session |
| Linux `v10` | AES-128-CBC, PBKDF2-SHA1, `saltysalt`, one iteration, legacy `peanuts` password | Real browser/version/keyring |
| Linux `v11` | Same parameters with browser-specific `secret-tool` secret | Real unlocked/locked session |
| macOS `v10` | AES-128-CBC, PBKDF2-SHA1, 1,003 iterations, exact browser Safe Storage service/account | Real browser, consent prompt/cancel, login/reboot |
| Windows legacy DPAPI | Original signed-in user only; no UI or elevation | Real supported browser/version |
| Windows `v10` | DPAPI-unwrapped 256-bit `Local State` key, AES-GCM nonce/tag authentication | Real pre-App-Bound profile and device/session validity |
| Windows `v20` | Explicit `APP_BOUND_UNSUPPORTED`; no key read or decryption attempt | Manual/provider route per real account |
| Other encrypted prefixes or future schemas | Explicit unsupported error | Format evidence and fixtures before implementation |

macOS pairs are `Chrome Safe Storage` / `Chrome`, `Chromium Safe Storage` /
`Chromium`, and `Brave Safe Storage` / `Brave`. Lookup reads only the selected
service/account, uses a bounded native helper and never creates an item. Denied,
cancelled, locked, absent and timed-out access return fixed errors without
helper output. No password is printed in diagnostics or passed as a process
argument. The importer requests a key only after finding eligible encrypted
cookies in one selected profile.

Windows cookie encryption is separate from provider Credential Manager entries.
Supported DPAPI reads require the original user and machine context. The backend
does not use administrator privileges, impersonate a browser, call its elevation
service, inject code, or weaken browser protections. Device-bound sessions may
also reject manually copied values; use the provider's supported OAuth/API route
when applicable. A raw cookie is not guaranteed to survive anti-bot challenges.
Existing T3 Chat full-cURL guidance remains available through `auth curl`.

The bundled provider adapter distinguishes recognized Cloudflare protection
pages from an expired login. A challenged request reports `failed` and a
Cloudflare message; it does not establish whether the saved session is valid.
This includes TypeSafe's console billing route, where a protection page could
previously be reported as “session expired.” Open the provider website to check
access. If browser access works but the backend remains blocked, use a
provider-supported automated access route; refreshing a cookie is not a
guaranteed fix. The adapter neither retries nor bypasses the protection.

TypeSafe still uses its console billing page and a discovered Next.js server
action with the selected cookie. The reviewed CodexBar and Win-CodexBar sources
have no browser-session transport that resolves Cloudflare challenges, and
crossusage/openusage have no TypeSafe provider to port. TypeSafe's
[published API schema](https://api.typesafe.ai/openapi.json), checked on
2026-09-29, lists inference and model discovery but no account billing/balance
endpoint. An API key therefore cannot replace this cookie path using the
documented API. Cloudflare detection improves the error message; successful
TypeSafe billing reads remain unverified and can remain blocked.

The database reader opens a read-only SQLite transaction with a 250 ms busy
limit. It reads committed WAL data directly, never marks an active store immutable,
and creates no temporary cookie database or sidecar copies. It filters exact
host/domain boundaries, URL path boundaries, HTTPS-only cookies, expiry (Chromium's
1601 epoch) and partitioned cookies. Version-24 ciphertext must start with the
SHA-256 digest of the exact stored domain. Earlier versions retain the entire
plaintext; JWT searches, guessed offsets and character stripping are removed.
Unknown schemas above 24 fail explicitly. Matching rows and data volume are bounded.

Errors retain the `error` / `message` JSON shape and include `BROWSER_NOT_FOUND`,
`PROFILE_NOT_FOUND`, `INVALID_PROFILE`, `AMBIGUOUS_PROFILE`, `SESSION_NOT_FOUND`,
`COOKIE_DB_UNAVAILABLE`, `COOKIE_SCHEMA_UNSUPPORTED`, `COOKIE_FORMAT_UNSUPPORTED`,
`APP_BOUND_UNSUPPORTED`, `KEYCHAIN_DENIED`, `KEYCHAIN_UNAVAILABLE`, and
`COOKIE_DECRYPT_FAILED`. Capability reports distinguish automatic import,
CBC, Windows DPAPI, unsupported App-Bound import, and provider-qualified manual
credentials. They do not inspect browser sessions or trigger Keychain prompts.

Source checks on 2026-09-07:
[macOS current Keychain derivation](https://github.com/chromium/chromium/blob/main/components/os_crypt/async/browser/keychain_key_provider.mm),
[Chrome/Chromium service/account](https://github.com/chromium/chromium/blob/131.0.6778.204/components/os_crypt/sync/keychain_password_mac.mm),
[Brave service/account](https://github.com/brave/brave-core/blob/master/chromium_src/components/os_crypt/common/keychain_password_mac.mm),
[Windows legacy DPAPI/AES-GCM format](https://github.com/chromium/chromium/blob/131.0.6778.204/components/os_crypt/sync/os_crypt_win.cc),
[cookie schema/domain digest](https://github.com/chromium/chromium/blob/main/net/extras/sqlite/sqlite_persistent_cookie_store.cc),
[Google's App-Bound design](https://security.googleblog.com/2024/07/improving-security-of-chrome-cookies-on.html).
