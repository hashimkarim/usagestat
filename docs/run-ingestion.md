# SDK run ingestion

Usagestat is the optional usage backend for AgenticDriver. The SDK reports execution measurements; this daemon owns their durable storage, account authorization, retention and optional forwarding. Existing provider probes, quota snapshots, daily imports, metadata and icons remain in Usagestat. Run records are separate observations and are not added to daily imports or quota totals, which could double-count the same activity.

Enable ingestion explicitly with a private configuration file:

```json
{
  "version": 1,
  "database": "run-usage/events.sqlite",
  "retentionDays": 30,
  "maxRecords": 100000,
  "maxBytes": 256000000,
  "clients": [
    {
      "token": { "file": "driver-ingestion.key" },
      "bindings": [
        {
          "hostId": "driver-installation-01",
          "provider": "openai-personal",
          "accountId": "account-personal-01",
          "subjects": ["authenticated-user-id"]
        }
      ]
    }
  ]
}
```

```sh
usagestatd --bind 127.0.0.1:6736 --run-usage-config /absolute/path/run-usage.json
# For a service used only for ingestion, disable all provider discovery and polling:
usagestatd --no-poll --run-usage-config /absolute/path/run-usage.json
```

Paths are relative to the configuration file. Use an independently generated credential of at least 32 ASCII characters, with no whitespace; it is neither a model API key nor the existing management API key. Credential references accept exactly `{"file":"path"}` or `{"env":"VARIABLE"}`. Configuration and credential files must be regular private files (0600 on Unix). Newly created database directories use 0700 and database files 0600; existing public directories/files are rejected. Windows uses the service account's inherited ACLs; configure those before deployment. Configuration is loaded on startup, so restart after changing bindings or rotating credentials. Duplicate credential values and unknown configuration fields are rejected.

With ingestion enabled, the daemon requires a loopback listener and caps concurrent HTTP handlers at 128. For remote access, put a verified HTTPS reverse proxy in front of the loopback listener and publish only `/v1/run-usage` and its subpaths. Keep the backend bearer authentication intact. The existing administrative read routes have their own access assumptions and must not be exposed as user-scoped ingestion routes. Browser `Origin` headers are rejected for ingestion, and these responses have no wildcard CORS headers.

## Versioned contract

All routes require `Authorization: Bearer <ingestion credential>` and return `Cache-Control: no-store`. POST records require `Content-Type: application/json` and a single unambiguous `Content-Length`. Chunked transfer, duplicate lengths, encoded bodies, oversized/incomplete headers and bodies are rejected. The request body limit is 65,536 bytes. A reverse proxy must preserve or calculate the bounded body length.

| Method and route | Result |
| --- | --- |
| `GET /v1/run-usage/protocol` | `usagestat.run-ingestion.v1`, accepted event schemas, receipt schema, size limit and required account flag |
| `POST /v1/run-usage` | 201 accepted or 200 duplicate, with `usagestat.run-receipt.v1` |
| `GET /v1/run-usage/{hostId}/{eventId}` | Authorized `usagestat.stored-run.v1`, including the record, effective expiry, delivery state, attempt count and redacted delivery error |
| `POST /v1/run-usage/{hostId}/{eventId}/retry` | Explicitly queue a retained failed forwarding attempt after repairing its cause; returns `usagestat.run-retry.v1` |

The supported event schema is **`agenticdriver.usage.v2`**, specified by `protocol/usage-record.schema.json` and `validateUsageRecord` in the SDK. Native ingestion additionally requires a bound `accountId`. Release qualification requires capturing and reconciling real AgenticDriver execution and embedding records against the actual release candidate binary; synthetic validation/storage tests alone do not establish this integration. See `docs/releases/v2.0.0-publication.md` for the recorded result. Envelope/receipt versions are checked explicitly, and unknown event versions are rejected rather than guessed or silently migrated.

Records contain matching UUID `eventId`/`runId`, host, account, provider, subject, model, authentication mode, measurement source, terminal status, start/finish time, duration, optional expiry, measurements, measurement coverage and bounded labels. No prompt, response, tool payload or credential field is accepted. SDK labels come from trusted host configuration; application request metadata is excluded by the SDK.

Missing measurements stay missing. `observedUsage` contains known subtotals and `coverage` explains their completeness; `usage` only contains complete measurements. Null, negative, fractional or unsafe token counts and inconsistent coverage are rejected. `apiEquivalentCostUsd` estimates are distinct from reported `costUsd`; subscription list-price estimates are not asserted to be actual billing. Never sum a cache subset into the inclusive input count again.

Each request must match all four configured identity dimensions: host, provider instance, account and authenticated application subject. Ingestion rejects a mismatched account with 403. Lookup and retry return 404 for records outside the credential's bindings, including other accounts on the same host. There is no provider-level identity fallback.

The stable key is `(hostId, eventId)`. Parsed records are canonically serialized and hashed inside a SQLite transaction. The same record returns its existing receipt without changing delivery state or expiry; changed content under that identity returns 409. A receipt is sent only after the local SQLite commit ([`synchronous=FULL`, WAL](https://www.sqlite.org/pragma.html#pragma_synchronous)) completes. An interrupted HTTP acknowledgement can therefore be reconciled with GET or retried using the same event, without replaying model/tool execution.

Errors use `usagestat.run-error.v1` and fixed codes: 400 invalid record/request, 401 unauthorized, 403 forbidden scope/origin, 404 missing/disabled/inaccessible, 409 identity conflict or invalid retry state, 410 expired event, 415 wrong content type, 429 store capacity, and 503 unavailable storage/connection capacity. Raw provider, database and credential diagnostics are not returned.

## Durable forwarding and retention

For offline recovery, run Usagestat locally and add an optional forwarding destination:

```json
{
  "forward": {
    "url": "https://usage.example.com",
    "token": { "file": "remote-ingestion.key" },
    "timeoutMs": 2000
  }
}
```

This fragment goes beside `clients` in the configuration. The remote Usagestat instance must authorize the same source identities. Forwarding accepts HTTPS origins with certificate verification, or explicit loopback IP HTTP origins for local connections. URL credentials, query strings, fragments, path prefixes and redirects are rejected. Environment proxy discovery is disabled. TLS termination is external to the native loopback listener.

Local capture is acknowledged independently of remote availability. Pending events and retry state live in the same native SQLite store, and survive process restart. Network failures, HTTP 408/429 and 5xx responses retry with capped exponential backoff (1–60 seconds). Delivery is marked `delivered` only after a valid peer receipt acknowledges that exact host and event. This means acknowledgement by the configured next backend; it does not imply that every backend in a forwarding chain has completed its own forwarding. A lost acknowledgement after the peer commits results in a duplicate receipt on retry.

Permanent authorization, conflict, expiry or protocol failures remain visible as `failed` with a fixed error code. Repair the configuration/destination and explicitly call the scoped retry endpoint; ordinary duplicate ingestion does not restart permanent failures. Already delivered records cannot be requeued by this route. A database with pending **or failed** deliveries cannot be rebound to a different destination. Disabling forwarding preserves pending state until it is enabled again or the records expire.

Capacity is bounded by retained record count and the sum of serialized payload bytes. `maxBytes` does not include SQLite pages/indexes/WAL or OS overhead, so operators still provision disk space. A full store rejects new events without evicting retained events; duplicate acknowledgements remain available. Backpressure never reruns model work. No global unbounded SDK outbox or separate SDK usage database is introduced.

Effective retention is the earlier of the record's optional `expiresAt` and `finishedAt + retentionDays` (default 30, range 1–3650 days). Expired incoming records return 410. Cleanup runs at startup, on reads/writes and periodically, including when no forwarding is configured. Pending events expire too; retention is not suspended by an offline destination. Deletion uses [SQLite secure deletion](https://www.sqlite.org/pragma.html#pragma_secure_delete) and WAL checkpoints; external backups/filesystem snapshots follow the operator's retention policy. This is logical application retention, not a forensic erasure guarantee. Already accepted records keep their receipt expiry; configuration changes apply to new acceptance. With no event-level expiry, a later deliberate policy increase can allow reimport of an old record after its earlier copy has been purged; callers requiring an immutable cutoff must set `expiresAt`.

Durability begins at acknowledgement by the first backend. A crash before the SDK emits the report, or an unavailable first-hop backend, cannot be described as durably captured. The SDK exposes capture failure separately from the execution outcome. Retain the original record in an application-owned workflow when that gap must be reconciled, or operate the local backend alongside the execution host. Transport timeouts, retry cadence and retention are housekeeping policies; none imposes a model run deadline or enables an inactivity timeout.

Run `cargo test -p usagestat-core -p usagestat-daemon` for validation, storage and actual-daemon tests covering scope, capacity, restart, offline recovery, lost acknowledgements and explicit recovery from permanent forwarding failure. The existing native CI runs these workspace tests on its platform matrix; only locally executed platforms should be described as verified before those jobs run.
