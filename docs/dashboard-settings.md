# Dashboard settings

Open **Settings** at `/dashboard#settings`. There are two scopes:

| Scope | Stored in | Effect |
| --- | --- | --- |
| Display · this browser | `usagestat.dashboard.prefs` in browser storage | Appearance, visible providers and trackers, names, ordering and interaction |
| Collection · shared backend | The daemon's existing private `config.toml` | Provider polling, sources, credentials and plugin discovery for every client of that daemon |

Hiding a provider or tracker does not stop collection. Pausing collection does
not hide or delete its saved usage. Paused sources show a saved-usage label.
History retains provider records after a source is paused, hidden or removed.
Dashboard display choices remain independent of UsageStat Bar's settings.

These features are included in v2.0.1. Upgrade the daemon to receive them;
the immutable v2.0.0 binaries and tag are unchanged.

## Display

Choose dark, light or system appearance, an accent or custom meter colour,
card density, provider spacing, vertical or horizontal meters, Used/Remaining
quota, and an independent view refresh interval. View refresh reads saved
observations; Collection sets how often providers are polled.

Each provider has its own visibility, name, pin/order, summary quota, visible
trackers, logo and icon style. Logo menus contain only the actual provider and
its related variants from the pinned shared catalogue: for example, Codex and
OpenAI, or Copilot and GitHub Copilot. Monochrome and colour are separate style
choices. A logo without a colour variant uses a tinted monochrome fallback.
Uploaded PNG/JPEG/WebP logos are limited to 380 KB and remain in display
preferences. **Use bundled logo** removes an upload.

Other controls include full or quota-following logo fill, card component
visibility/order, pace, reset times and their format, provider status/usage
links, and scrolling on tabs to switch providers. Usage thresholds have editable
names, percentages used, colours and crossing notifications. Browser permission
is requested only by **Enable browser notifications**; notifications can also
be disabled. Only a new, fresh, successful observation of the same quota window
can trigger a crossing notification. Keyboard shortcuts apply while this page
is active and do not intercept form input or OS shortcuts.

These are the dashboard counterparts of Bar's display controls. GNOME panel
placement/index, extension shortcuts outside the page, and local CLI executable
paths remain Bar/desktop settings.

The summary quota can use an actual percent, credit or token budget with a
positive reported limit. A hidden, absent or unlimited budget never creates a
quota. Explicitly hidden providers stay hidden when inactive providers are shown.
Overview totals follow manual visibility; saved History remains available.

**Export settings** and **Import settings** transfer validated display
preferences, including custom logos. Provider credentials and collection
configuration are excluded. Other tabs at the same origin adopt saved changes.
**Reset dashboard** offers an undo until reload. Blocked browser storage applies
preferences for the current session and displays a warning.

## Collection

Open the dashboard directly on the local daemon's loopback address to edit
collection. Remote listeners and ingestion-only (`--no-poll`) daemons do not
expose this setup API. Choose **Choose local setup key** and select the
`dashboard-setup.key` file at the path shown in Collection settings. The daemon
creates it privately in its data directory. This proves access to the local
profile: loopback networking alone cannot distinguish OS users.

The private key is never returned by HTTP or saved in dashboard preferences.
Only a per-launch capability is retained in this tab's origin/port-scoped
`sessionStorage`, so reloading the page preserves access until the backend
restarts. Select the private file again after a restart. T3 management,
lifecycle and SDK bearer credentials are separate and are not expanded or
migrated. The [local setup API contract](dashboard-collection-api.md) describes
the transport boundary and fields.

Enable/pause a source, choose an advertised source mode, set a shared source
name, API key/token, session cookie header, region, workspace/project ID, or a
custom usage command. Provider-specific text, number and boolean settings can
be added by name. Auto retains the provider's normal credential discovery.
Supported modes depend on the plugin. Provider sign-in and browser cookie
extraction are still performed by the provider or its existing local tools;
saving a cookie is not a Cloudflare bypass or an application login.
Enabled Custom commands execute with the daemon user's local permissions.

Saved credentials and custom commands are write-only. An empty secret field
preserves its saved value; **Clear saved value** explicitly removes it. Secret
settings and existing structured settings show only that a value is saved.
Structured values are preserved unless explicitly replaced or cleared; this
form edits primitive settings rather than a raw JSON document.

**Add another account or source** creates a unique instance, paused initially.
Configure its actual credentials/account selectors before enabling it. Each
instance has an independent snapshot, history ID, daily ingestion ID and plugin
storage directory. Merely adding a name does not discover a different account:
local tools that expose one logged-in profile still require a supported account
selector or a distinct profile configuration. This does not create an SDK
execution-host/account/subject binding or grant.

Polling and plugin directories can be changed from the form. Launch-time polling
overrides are shown and cannot be replaced by the form. Changes reload during
the next polling cycle without a daemon restart; an in-flight provider probe can
finish first. External Bar/preferences file changes are also detected.

Writes preserve other providers, unknown configuration fields and untouched
secrets. A revision check rejects an outdated form when the file has changed;
reload before saving again. HTTP writes are serialized and use private atomic
file replacement. Unsaved forms survive background refreshes, switching the two
settings scopes, and saving another form. Pending drafts live only in this page
and are discarded by a full page reload. Explicit **Reload collection settings**
discards drafts and loads the current configuration.

## Chart details

Quota meters/arcs, pressure and contributor bars, overview trends, provider
sparklines, daily charts, calendars, model mixes, snapshot histories and cost
stacks have hover details. Focus or tap also opens details. Daily and snapshot
line charts support arrow-key inspection; Escape closes the tooltip. Tooltips
are clamped to the viewport and can be scrolled when long.

Details include available used/limit/remaining values, reset/window times,
observation state/source, input/output/reasoning/cache/total tokens, cost,
component costs, pricing provenance, sessions and cache savings. Very small
costs retain their precision in tooltips. A historical point uses its own
recorded data; current quota is not silently attached to an earlier cost day.
Missing fields stay unknown, missing days stay distinct from measured zero,
and partial/unpriced cost flags remain explicit. Snapshot counters are not
summed as new usage. Local log costs at API-equivalent rates are separate from
subscription charges, and estimated stack splits are labelled.

## Antigravity access errors

A Google HTTP 403 quota denial differs from an expired access token. Antigravity
still tries supported model quota fallbacks; if access remains denied the
dashboard reports Google's quota denial for the account. Open the latest
Antigravity/agy client and check account access. Usagestat cannot grant a Google
license or infer missing quota. HTTP 401 can still trigger one normal OAuth
refresh.
