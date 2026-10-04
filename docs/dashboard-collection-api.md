# Local dashboard collection API

This additive `/v1` API is included in v2.0.1. It does
not change native usage readback, SDK run ingestion, T3 management or their
credentials. It is available only on loopback listeners with polling enabled.
Remote listeners and `--no-poll` installations return 404 for setup routes.

## Local owner capability

All requests require `X-Usagestat-Dashboard: 1` and an exact loopback Host with
the listening port. A supplied Origin must equal `http://<Host>`; mutations
require that Origin. Cross-site fetches and ambiguous duplicate headers are
rejected. Responses are uncached and do not grant CORS access.

The polling daemon creates a random `dashboard-setup.key` in its private data
directory using private create-once storage. Unix files are owned by the daemon
user and mode 0600; Windows uses the existing private state ACLs. The key
persists across restarts and is distinct from every provider, T3 and SDK key.
Only the OS profile owner can obtain it through the normal filesystem boundary.

`GET /v1/settings/session` requires `X-Usagestat-Setup-Key` containing this key,
or an already valid `X-Usagestat-Session`. It returns a random per-launch
`token`. Without proof it returns 401 with `error: dashboard_setup_key_required`
and the `keyFile` path; no key or capability is returned. Other setup requests
require `X-Usagestat-Session` containing the per-launch token. Restarting the
daemon invalidates it. The browser uploads the private file to bootstrap access;
it retains only the launch capability in tab-local sessionStorage, never the
private file contents, persistent display storage, an export or a URL.

## Read and edit

`GET /v1/settings` returns `schemaVersion: 1`, an opaque `revision`, saved and
effective polling intervals, any launch override, additional plugin directories,
configured provider instances and a loaded plugin catalogue. The catalogue
contains provider IDs, names and supported/default modes.

Version 2.0.2 also advertises optional `setupHelp` text and `setupFields`
entries with `key`, `title`, `description` and primitive `type`. These are
provider setup hints, not credential values or new API fields for mutations.
The dashboard saves supplied values through the existing provider `settings`
patch and omits untouched blank suggestions. Existing clients can ignore this
additive catalogue metadata.

Provider entries include `id`, optional `instanceId`, `displayName`, `enabled`,
`source`, `region`, `workspaceId` and existing grouping metadata. `apiKey`,
`cookieHeader` and `customCommand` are replaced by `...Configured` booleans.
Provider-specific settings return typed primitive values unless their name
marks them as secret. Secret and structured settings return only their type
and configured/secret flags. Unknown root/provider fields are retained on disk
and are not exposed as an unfiltered configuration document.

`PATCH /v1/settings` requires `Content-Type: application/json` and the latest
`revision`. Supported edits are:

| Field | Meaning |
| --- | --- |
| `refreshSec` | Integer from 5 to 86400; a launch override still wins |
| `pluginDirs` | Up to 32 additional paths; normal discovery remains active |
| `provider` | Edit one provider/instance by `id` and optional `instanceId` |
| `removeProvider` | Remove one explicit instance by its `instanceId` |

Provider edits accept `displayName`, `enabled`, an advertised `source` or
`custom`, `apiKey`, `cookieHeader`, `region`, `workspaceId`, `customCommand` and
named primitive `settings`. Omitted fields preserve saved values. Null clears
an optional value or named setting. IDs, fields, types, lengths, unique instance
identities and enabled custom commands are validated before publication. New
entries default to paused; the dashboard creates a unique ID and leaves
credential/account selection to an explicit subsequent edit.

Writes serialize within this daemon, check the file revision again before
replacement, preserve unknown fields and untouched secrets, and atomically
replace the private configuration. A stale form receives 409 `config_changed`;
the browser never retries it automatically. Invalid values return 400, incorrect
content type 415 and unavailable private configuration/storage 503. Errors never
include credential or custom-command contents. Polling reloads the saved
configuration without a restart; an in-flight probe may finish first. Changes
from other local configuration writers are detected too.

## Instance and history compatibility

Configured instances use their own provider IDs for snapshots, snapshot history,
daily accounting ingestion and plugin storage. Their `/v1/providers` rows add
an optional `pluginId` identifying the original plugin; existing base rows keep
their established shape. Existing `tabParent` metadata is preserved. A source
name alone does not select a new provider account or create an SDK identity grant.
Pausing or removing a source preserves recorded history.

New snapshot and compact history records optionally include `source` and
`state`. Older records remain readable. Missing or failed quota observations
remain unavailable; counters in snapshots are not added together as new usage.

`GET /v1/icon-catalog` returns the pinned catalogue's IDs, aliases, names,
related alternatives and monochrome/colour availability, without filesystem
paths. `/v1/icons/:id?style=monochrome|color&source=<catalogue-id>` serves only
registered or pinned SVG files. The dashboard restricts the chooser and imported
selections to the actual provider's published family. A colourless logo falls
back to monochrome. The shared provider-icons pin remains v0.1.0-alpha.1.
