# Provider sources

A quota, prepaid balance, spending cap and billed charge answer different questions.
UsageStat preserves the source’s units and confidence. A missing measurement stays
unavailable; a live estimate does not become retained billing history. The
[generated inventory](provider-compatibility.md) records implemented sources and
fixture coverage. These fixtures do not qualify a real account or browser session.

## Choose the billing scope

Provider-specific settings under **Settings → Collection** select credentials and
billing scope. Some services need an organization, project, workspace or key ID in
addition to the credential. Secondary secrets belong in the provider’s own setup
fields. Bundled providers and native scoped cookie sessions require additional named
accounts to use saved settings rather than default-account environment credentials.

Vercel’s default source reports AI Gateway balance. **Personal budget** instead
uses a separately configured Vercel access token and the selected user or team.
QwenCloud’s **Team Token Plan** is an explicit alternative to its individual
subscription. An empty alternative-source response never authorizes a fallback
to a different account or billing scope.

## Manual web sessions

Web providers can require a saved cookie header from a signed-in browser. This
is separate from automatic browser import. Native scoped cookie sessions attach
credentials only to reviewed HTTPS routes; setup and the inventory identify
providers using this mechanism. Disabling cookie access disables that path.

Gemini Apps uses its GraphQL usage endpoint and retains its existing provider
identity and `GEMINI_COOKIE` alias. X API and xAI are separate products with
separate billing sources. Keep their credentials in their own entries.

## Local application observations

Cursor safe-storage values are ciphertext, so they are never sent as access tokens.
The default stable profile may use the Cursor CLI login from `agent login`; Nightly,
explicit database profiles and named accounts do not borrow that login. CLI-owned
credentials are read without modifying the CLI file.

JetBrains keeps explicit IDE selection authoritative. Optional **Read local Junie
observations** can consult running IDE quotas and bounded local logs for the default
account; it is disabled when a specific configuration directory is selected. It
neither refreshes JetBrains cloud tokens nor changes stored credentials.

Kimi monthly detail divides one measured monthly pool into Code and other-feature
shares only when both figures come from that same source. Codex quotas retain the
reported window duration, including the fixed 30-day Free window.

## Retained accounting

Cursor’s event collector accepts reported charges only after all requested pages
are complete. Its private derived cache is scoped to the account, selected profile
and timezone. Missing charge evidence or incomplete backfill uses the existing
CSV export. Upstream list-price estimates are not imported as billed charges.

Claude reset-grant inventory is checked hourly while ordinary quota reads continue
between checks. Grant expiry and account boundaries still apply to cached data.
Cloud credits and optional xAI invoice previews enrich the live card without
inventing a daily charge.
