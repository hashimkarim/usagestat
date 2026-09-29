# Daily history and local estimates

Daily history keeps one selected source per provider and UTC date. Billing rows
take precedence over local transcript API estimates, which take precedence over
ccusage rows. A provider-day with local transcript data selects that whole row,
even when some model costs are unknown; ccusage is the fallback when local data
is absent. A newer estimate does not overwrite a billing value. All source rows
remain retained for provenance, and `includeSources` marks the selected source.
The local Claude rows use message/request deduplication introduced in cache-v5.

The dashboard headline uses this selected daily history. The Model table uses
saved daily rows with model detail, applying the same source precedence among
rows that actually contain models. Its source or coverage can differ from the
headline; the table reports that difference instead of scaling estimates to fit
the headline. Per-model costs are not inferred from a provider's total cost.

## Saved fields

`usage_daily.json` rows retain `providerId`, `date`, `source`, `costSource`, `pricingAsOf`,
`models`, `sessions`, `cacheSavingsUsd`, and optional `timeZone`, along with the existing token and
cost fields. `models` is keyed by model ID and contains the token breakdown,
`costUsd`, `costKnown`, `tokensKnown`, optional `sessions`, and optional
`cacheSavingsUsd`. Ingestion accepts both a `models` object and a
`modelBreakdowns` array. These fields are additive and old stores remain readable.

Daily rows and each `models[id]` can also contain `inputCostUsd`,
`cacheReadCostUsd`, `cacheWriteCostUsd`, and `outputCostUsd`. Locally priced rows
calculate these per event before applying daily/model aggregation. All four are
present, including explicit zeroes, only for a complete known breakdown; their sum
matches `costUsd` within floating-point tolerance. They are an exact allocation of
the **API estimate**, not evidence of an exact invoice. Mixed unpriced usage,
legacy rows, and reports without component costs omit all four. Ingestion rejects
incomplete, negative, or mismatched breakdowns. Weekly/monthly and cost totals
preserve components only when every contributing row has a complete breakdown.
`/v1/history/daily`, `/v1/history/models/<provider>`, and `/v1/cost/<provider>`
pass them through, as do local session/block reports. CSV uses the snake_case
column names `input_cost_usd`, `cache_read_cost_usd`, `cache_write_cost_usd`, and
`output_cost_usd`; absent components export as empty cells.

Input classes are mutually exclusive: uncached input + cache reads + cache
writes. Output includes reasoning. Codex's inclusive input counts are normalized
at parsing time. Repeated cumulative snapshots contribute zero; ordinary and
subagent rollouts retain counter-reset and explicit inherited-history handling.
Only session metadata supplies a Codex session ID; message IDs do not.

Claude local estimates suppress identical model/token usage repeated under the same
session, message ID and request ID. Distinct messages sharing a request remain
separate. Without that pair, only an identical accounting payload with the same
record UUID is deduplicated; records without a usable identity remain separate.
This deduplication is per transcript file. Changed usage for an existing identity
is retained rather than assumed to be a duplicate. Local-cache version 5 forces
unchanged files to be reparsed and replaces local-transcript summaries; saved
ccusage/billing rows and source precedence are unaffected.

New native ccusage queries explicitly pass `--timezone` using `timeZone` from
query options, otherwise `TZ`, otherwise the detected system IANA zone (UTC if
unavailable). The exact requested zone is carried into each returned daily row
and persisted at ingestion. This pins the grouping zone rather than guessing it
later; the explicit zone overrides ccusage's own config-file timezone. Existing
saved/imported rows without timezone metadata stay unlabelled until replaced by
a fresh report that supplies it. Local transcript reports explicitly supply UTC.
`ingestedAt` is the time a stored source row last changed, not a guarantee of
complete coverage through that instant or the time of the latest unchanged poll.

Missing costs remain unknown, including models that have no verified price.
Explicitly known zero cost remains zero. Missing session counts or savings remain
absent, rather than being recorded as zero. A session counts once per provider
and active UTC day (and once for each model it used that day). Summing daily
counts across a period yields **session-days**, not distinct sessions over the
whole period; model session counts also cannot be added to get provider counts.

For supplemental sessions or savings when the selected headline source lacks
them, read the separate daily model report and retain its `source`, `costSource`,
and date coverage. Sum provider-day `sessions`, not the per-model counts. Display
local values as **local session-days** and **estimated local cache read savings**,
with the covered-day count, independently of the headline source. Do not attach
them to a billing/ccusage row or use them to imply matching source coverage.
Savings are complete only when every included row has a finite savings value;
unpriced models must not become zero savings. The model report chooses the best
available source with model detail, so inspect its source rather than assuming
it will always be a local transcript.

Legacy daily totals that exactly equal the exclusive component sum plus reasoning
are corrected on read. Other reported totals are preserved. Historical costs are
not silently repriced.

## Local report cache

The daemon prepares Codex and Claude daily model rows in a background worker for
enabled providers. The first pass still needs to read existing local logs.
HTTP history reads serve already saved rows while it runs. The worker refreshes
about once a minute; direct local-report requests check at most every 30 seconds.

Each profile has a separate private `local-usage-<provider>-<profile-hash>.json`
cache in the daemon data directory. It stores token/cost summaries, session IDs,
model IDs, timestamps and project labels, not conversation text. Unchanged files
are reused by path, size and modification time, including after restart. Added,
changed, removed or truncated files invalidate the relevant cached summaries.
Concurrent requests share the same scan. Serialized report variants are cached
as well. Deleting the cache is safe; the next pass rebuilds it. Increment the
cache version when changing parsing or pricing semantics.

Saved historical days remain available after their original logs are removed.
Older days without model detail cannot be reconstructed from the aggregate daily
total alone; they need the original logs or a fresh upstream daily breakdown.

## HTTP and dashboard

- `GET /v1/history/daily?includeSources=true` and
  `/v1/history/daily/<provider>?includeSources=true` add `sourceRows` only for
  provider-days with multiple retained sources. Each compact entry contains
  `source`, `selected` (exactly one true), `ingestedAt`, optional `timeZone`,
  optional `costSource`/`pricingAsOf`,
  `costUsd`, `costKnown`, `totalTokens`, `tokensKnown`, `inputTokens`,
  `cacheReadTokens`, `cacheCreationTokens`, and `outputTokens`. The selected row
  and alternatives come from one store read, with no transcript scan or repricing;
  default responses are unchanged. Compare only known costs and retain token
  coverage differences; these sources need not represent the same usage or an invoice.

- `GET /v1/history/models/<provider>` returns saved daily model rows without
  scanning session logs. The UI refreshes these on its normal refresh cycle.
- `GET /v1/history/<provider>?since=2026-09-01&until=2026-09-27&group=hour&view=chart`
  bounds polling history by inclusive UTC dates. `group` accepts `raw`, `hour`,
  `day`, `week` (Monday), and `month`. Grouping uses the latest counters and peak
  quota values, never a sum of polling snapshots. `view=chart` omits text, badges
  and embedded charts. The same options work for `/v1/history` and
  `/v1/history/quota[/<provider>]`. No-query clients keep the full array schema.
- `GET /v1/local-usage/<provider>/session?limit=80` and the corresponding `blocks`
  route accept a limit from 1–1000 and return `totalRows`. Unbounded requests
  remain supported. Session rows are ordered by estimated cost; blocks are newest
  first. All daemon JSON responses are compact.

Provider tabs request the last 30 days of hourly chart summaries. Sessions and
Blocks are fetched only when opened, capped at the 80 rows the tables display.
The History Model view never downloads a session report or assigns an entire
session to its last active day.

## Estimate pricing

Local `costSource: "api-rate-estimate"` values use standard API rates verified on
2026-09-27, with Sonnet 5.5 added from its published rates on 2026-09-29.
The table is dated 2026-09-29. Local-cache version 6 reparses unchanged logs to
include this new model; retained billing and ccusage rows are unaffected.
The dated current rate schedule is applied to local logs, including
historical events; it does not reconstruct past price changes. Saved billing and
ccusage costs are retained. These estimates represent API-equivalent usage, not subscription charges or an
invoice. `costKnown` means that the model's base rates are available; it does not
claim knowledge of account discounts, service tiers, region premiums or every
provider-specific cache-retention modifier. OpenAI long-context multipliers are
applied per usage event before aggregation. Claude cache writes use the standard
five-minute rate. `cacheSavingsUsd` measures the read discount relative to normal
input pricing; cache-write premiums remain included in `costUsd`.

Prices are matched to explicit model IDs (with optional snapshot-date suffixes).
Unknown aliases remain unpriced. Sources:

- [OpenAI pricing](https://developers.openai.com/api/docs/pricing)
- [GPT-6 Astra](https://developers.openai.com/api/docs/models/gpt-6-astra)
- [GPT-5.6 Sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol)
- [GPT-5.6 Terra](https://developers.openai.com/api/docs/models/gpt-5.6-terra)
- [GPT-5.6 Luna](https://developers.openai.com/api/docs/models/gpt-5.6-luna)
- [GPT-5.5](https://developers.openai.com/api/docs/models/gpt-5.5)
- [GPT-5.4](https://developers.openai.com/api/docs/models/gpt-5.4)
- [GPT-5](https://developers.openai.com/api/docs/models/gpt-5)
- [Claude pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- [Claude Sonnet 5.5](https://www.anthropic.com/claude-sonnet-5-5)

## Verification

```sh
cargo test -p usagestat-core -p usagestat-daemon
cargo build -p usagestat-daemon
node --test crates/ai-usage-daemon/tests/*.test.cjs
```

The HTTP integration test uses synthetic transcripts and an isolated daemon
profile, with a local fixture plugin and no provider network requests.
