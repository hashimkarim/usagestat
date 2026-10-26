# Provider sync rationale

UsageStat incorporates provider behavior from the reviewed inspiration repositories
without adopting their desktop shells, account-switching services or remote account
management. The local dated sync report records the exact upstream revisions.
The public provider inventory remains the source for supported mechanisms and
qualification limits; synthetic fixtures do not establish live account acceptance.

## Ownership and identity

Existing provider IDs are persistence boundaries: configurations, retained history
and SDK bindings refer to them. Alternate billing sources therefore stay under
the existing identity. Explicit Vercel personal budgets require a separate access token. Qwen team
Token Plans use the configured Qwen session at a different billing scope; an
empty response cannot authorize silently switching sources.

The bundled adapter translates upstream definitions into UsageStat metrics and
bounded host operations. Source composition uses provider factories rather than
a second runtime registration system. Immutable upstream bytes, hashes and
licenses remain owned by `tools/vendor-codexbar-providers.cjs`; generated fetchers
should be regenerated through that owner. Native cookie injection remains in
`cookie_sessions.rs`, where routes, methods and CSRF headers are constrained.
Manual cookie support does not imply automatic browser discovery.

The bundled providers and native scoped cookie sessions require named accounts
to supply saved credentials; ambient default credentials cannot fill a missing
secret in these paths. The new providers are disabled by
default so expanding the catalog does not start collecting additional accounts.

## Evidence and accounting

A balance, spending cap, quota, billed charge and estimate have different meanings.
Unknown values remain unavailable. Optional readbacks can enrich a successful
primary result without turning an endpoint failure into a fabricated zero.
Claude's reset-grant inventory has a persisted hourly cadence; ordinary quota
reads continue between checks and expired grants are excluded from the readout.

Cursor's upstream event history cannot be adopted with list-price amounts stored
as invoices. The native collector requires complete pages with reported charges,
rejects overlapping pages, and falls back to the existing CSV source when evidence
is missing. Completed derived days survive restart; today's persisted partial
snapshot cannot hide a failed refresh. Cache identity includes the selected
account, profile and timezone, while short-lived request caches also include the
credential revision. No application database migration is needed.

The OpenCode v2 credential table's presence was an incorrect proxy for import
completion. Its migration marker determines when that table becomes authoritative;
a pre-import empty table must not suppress live `auth.json`, while a completed
import followed by logout must not resurrect it.

## Boundaries and verification

The sync preserves the existing transcript collectors and their reconciliation
rules. The reference applications' Swift tail-parser infrastructure is not a
replacement for UsageStat's collectors. Price tables require primary provider
verification and are not copied from reference estimates. Automatic reset claims,
sign-in switching and desktop UI machinery remain outside this provider sync.

The implementation lives in `crates/ai-usage-plugins/src/bundled_provider.js`,
`cookie_sessions.rs`, `cursor_usage_export.rs`, `cursor_usage_export/events.rs`
and the existing provider entries.
`tests/provider-inspo-20261026.test.cjs` exercises the shipped entries, while
`tests/bundled-providers.test.cjs` verifies upstream provenance. Native QuickJS,
cookie-route and local HTTP pagination tests cover their host boundaries.
The provider audit generator derives its bundled catalog from the manifests.
There is no visual redesign or image comparison requirement in this change.
