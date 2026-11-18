# Provider incident status

The daemon reads live incident status for enabled providers whose status page
is a Statuspage.io site, and serves it at `GET /v1/status`. The dashboard shows
a badge on the provider's overview card and tab whenever the page reports
anything other than "operational", linking to the status page.

## What is monitored

Only status hosts verified to serve the Statuspage `api/v2/summary.json`
document at their own origin are polled; the table lives in
`crates/ai-usage-daemon/src/status_monitor.rs`. Every other status page,
including pages that redirect to another host or serve a different format, is
reported as not monitored. Nothing is inferred from a page that was not read.

To monitor a new host, confirm that `https://<host>/api/v2/summary.json`
returns a Statuspage document without redirecting, then add it to the table.

## Polling

- Independent of quota polling: each status origin is read every 5 minutes.
- A failure retries after 5 minutes, then backs off to 10, 20 and 40 minutes,
  capped at one hour.
- One request per distinct origin, however many providers share it.
- Anonymous HTTPS GET with a 10 second timeout: no credentials, cookies or
  redirects. Responses over 2 MiB are rejected.
- `USAGESTAT_STATUS_CHECKS=0` disables status requests entirely; every row then
  reports `monitored: false`.

## `GET /v1/status`

One row per enabled provider, in provider order:

```json
[
  {
    "providerId": "claude",
    "statusPageUrl": "https://status.anthropic.com/",
    "monitored": true,
    "state": "ok",
    "indicator": "minor",
    "description": "Partially Degraded Service",
    "incidents": [
      {"name": "Elevated errors", "status": "investigating", "impact": "minor",
       "updatedAt": "2026-11-18T17:00:00Z"}
    ],
    "pageUpdatedAt": "2026-11-18T17:14:55Z",
    "checkedAt": "2026-11-18T17:15:02Z"
  },
  {"providerId": "cursor", "statusPageUrl": "https://status.cursor.com/", "monitored": false}
]
```

- `indicator`: `none`, `minor`, `major`, `critical` or `maintenance`, present
  only once a page has been read successfully.
- `state`: `pending` (not read yet), `ok`, `stale` (the latest read failed; the
  previous result is kept and `error` says why) or `unavailable` (no read has
  succeeded yet).
- `incidents`: unresolved incidents and in-progress maintenance, at most 10.
- `error`: a short category such as `timeout`, `redirect`, `http_503` or
  `not_statuspage`; response bodies are never echoed.
