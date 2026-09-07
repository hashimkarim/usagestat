# Native credential stores

Tracking: [credential semantics #10](https://github.com/hashimkarim/usagestat/issues/10)
and [provider mappings #11](https://github.com/hashimkarim/usagestat/issues/11).
Implementation is on the feature branch; credentialed provider qualification
remains pending. No live provider credential was read to implement these changes.

On Windows, the generic-password host methods use the supplied service string as
an **exact Credential Manager target**, with UTF-8 blob bytes. An optional account
must exactly match the stored username. There is no enumeration, prefix search,
or fallback to another account. Current-user methods obtain the native token's
username rather than trusting `USER` or `USERNAME` environment overrides.

Applications choose their own target names and payload bytes. Providers with
verified Windows mappings can call these additive methods:

```js
host.keychain.readWindowsGenericPassword(target, accountOrNull, 'utf16le')
host.keychain.writeWindowsGenericPassword(target, accountOrNull, text, 'utf16le')
```

Encoding is explicitly `utf8` or `utf16le`; JSON, raw and base64 strings remain
unchanged. No encoding heuristic is applied. Refresh replaces only the blob and
retains the existing target, username, persistence, attributes, comment and alias.
An account mismatch or oversized payload leaves the entry unchanged. New records
use current-user local-machine persistence (not a machine-wide credential).
Windows supplies no compare-and-swap operation for a competing application's
refresh; reads, writes and deletes within this process are serialized.

`host.keychain.capabilities` reports `genericPassword`, `genericItemAccount`,
`internetPassword`, and `windowsExactTarget`. Internet-password lookup remains a
macOS operation. Missing, mismatched, denied, unavailable-session, malformed,
unsupported, and other native errors have distinct diagnostic prefixes. The host
no longer labels every keychain failure as an item-not-found error.

## Upstream format evidence

Evidence was inspected on 2026-09-07. These are source mappings, not live-account
verification or a claim that all auth modes in the provider matrix are supported.

| Upstream | Windows target | Blob | Integration status |
| --- | --- | --- | --- |
| Codex direct keyring storage | `cli\|<first 16 SHA-256 hex digits of canonical CODEX_HOME>.Codex Auth`; username is `cli\|<hash>` | UTF-16LE JSON via keyring-rs 3.6 | Native direct/encrypted fixtures pass; see [Codex authentication](codex-authentication.md) for source selection and refresh limits |
| keyring-rs 3.6 | `<username>.<service>` unless an explicit target is supplied | `set_password` uses UTF-16; raw secret methods differ | Exact target and explicit encoding supported by host |
| go-keyring Windows implementation | `<service>:<username>` | UTF-8 bytes | Library format established; each consuming provider/version still needs verification |

Sources: [Codex auth storage](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/storage.rs),
[Codex keyring adapter](https://github.com/openai/codex/blob/main/codex-rs/keyring-store/src/lib.rs),
[keyring-rs 3.6.3 Windows backend](https://github.com/open-source-cooperative/keyring-rs/blob/v3.6.3/src/windows.rs),
[go-keyring Windows backend](https://github.com/zalando/go-keyring/blob/master/keyring_windows.go).

Native tests create unique disposable entries and delete only those targets.
They exercise non-ASCII payloads, both encodings, exact account matching, metadata
retention, failed updates and the JavaScript host methods. Tests use synthetic
payloads; native provider login, locked/denied stores and refresh behavior still
need consenting test-account qualification.

The native record and JavaScript integration tests pass on Windows Server 2025
x64 in [run 34075747116](https://github.com/hashimkarim/usagestat/actions/runs/34075747116)
(2026-09-07); regression tests also pass on both Linux and macOS architectures.

Later concurrent tests reproduced an intermittent missing immediate read and a
metadata mismatch on Windows. Serializing reads with mutations and keeping raw
fixture metadata operations under the same lock fixed the observed failures.
[Run 34094685300](https://github.com/hashimkarim/usagestat/actions/runs/34094685300)
passed four concurrent workers with 16 create/read/delete cycles each, 16 fresh
JavaScript host contexts, metadata retention and the complete five-target gate.
The same source passed native release workspace tests in
[run 34094784327](https://github.com/hashimkarim/usagestat/actions/runs/34094784327).
No retry hides a missing immediate read. These results do not establish behavior
under a competing application's credential refresh or a locked desktop session.

Microsoft documents [credential lookup in the current logon session](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credreadw)
and [application-defined target/blob semantics](https://learn.microsoft.com/en-us/windows/win32/api/wincred/ns-wincred-credentialw).
