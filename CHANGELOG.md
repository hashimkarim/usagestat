# Changelog

## 2.3.1 - 2026-11-19

- A managed daemon now restarts itself into an upgraded executable. It checks its installed path once a minute and, after a replacement has settled for 30 seconds and reports a `usagestatd` version, shuts down normally and re-executes in place with the same process, settings and autostart. This works for every package format that replaces the file in place. Upgrading to this version still needs one manual restart; Homebrew needs `usagestat daemon relocate`.
- Remove the RPM post-transaction restart hook (`usagestat-package-sync.service`, `service-sync.py`, `rpm-service-sync.py`) and the RPM's `python3` dependency. On SELinux-enforcing Fedora its root-to-user-session call failed silently, so it never restarted the daemon.

See [release notes](docs/releases/v2.3.1.md).

## 2.3.0 - 2026-11-19

- Add time span and grouping controls to dashboard charts. Usage History plots saved local tokens, cost and cache summed per day, week or month; Cost History, the History page and the activity heatmap gain span controls.
- Align chart axis ticks to calendar periods, adding years where ticks cross one. Mark missing data in line charts with hatched bands and dashed bridges.
- Reprice saved local requests on pricing upgrades without discarding token counters. Add GPT-5 Codex, GPT-5.2 and GPT-5.4 mini rates and Haiku prompt-length tiers. Retained daily rows whose logs are gone fill missing model costs at base rates, labelled `api-rate-aggregate-estimate`.
- Attribute Codex turns whose model is recorded later from that exact turn's metadata; ambiguous turns stay unattributed.
- Add spend alerts: an optional monthly API-equivalent budget and a daily spike alert relative to the 30-day average, shown on the Overview and optionally as browser notifications. The Overview and month-to-date Cost History show a projected month total.
- Add CSV export to Cost History and Usage History, matching each chart's span and grouping. Exported text that a spreadsheet would run as a formula is kept inert.
- Add `usagestat statusline`: one line of quotas, reset times and today's cost from the running daemon, for Claude Code and other agent statuslines.
- Add Projects and Hours panels to provider tabs: cost per project, and usage by hour of day and weekday in local time. The local log cache now keeps hourly detail (cache v10, rebuilt once from logs).
- Add Gemini CLI and Amp local log sources for token and cost history, alongside Claude Code and Codex. Gemini models stay unpriced until rates are verified. Startup at login keeps `GEMINI_DATA_DIR`, `GEMINI_CLI_HOME` and `AMP_DATA_DIR`.
- Add event hooks: `[[hooks]]` in `config.toml` runs a command or sends a webhook POST when a quota crosses a threshold, resets, or a provider starts failing or recovers. `usagestat hooks test <event>` sends a sample payload. See [docs/cli.md](docs/cli.md#event-hooks).
- Show live provider incidents from Statuspage.io status pages (Claude, OpenAI/Codex, Copilot, Droid, Augment, Deepgram, ElevenLabs, ZoomMate) as badges on cards and provider tabs, via the new `/v1/status` route. Checks run every 5 minutes without credentials; `USAGESTAT_STATUS_CHECKS=0` turns them off. See [docs/provider-status.md](docs/provider-status.md).
- Add a provisioning-ready Grafana dashboard for `/metrics` in [docs/grafana](docs/grafana/README.md).
- Add `usagestat project`: local agent cost and tokens for the repository you're in, per agent and in total. Projects now resolve live subfolders and linked git worktrees to their main repository (cache v12, rebuilt once).
- Add optional multi-machine totals: with `[sync] dir = …`, each daemon publishes daily cost per project to a shared folder and the Overview (and `/v1/machines`) shows every machine and the total.
- Add `usagestat top`, a live terminal view of quotas, today's cost and active sessions. Session reports accept `order=recent`.
- Add `usagestat mcp`, an opt-in MCP server on stdio with read-only tools for quota status, repository cost, costliest sessions and model what-ifs. It runs only when an agent starts it.
- Add optional `[[budgets]]`: monthly API-equivalent limits per project or overall, shown by `usagestat project` and alerted through the new `budget_threshold` hook event. The daemon only reads logs for budgets when a hook subscribes.
- Add `usagestat whatif <model>` and `/v1/local-usage/<provider>/whatif`: recost local usage at another model's base API rates.
- Add `usagestat sessions`: the costliest agent sessions across Claude Code, Codex, Gemini CLI and Amp, filterable by days and project. Session reports gain first activity and branch; the Sessions tab shows branch, main model and duration.
- Add plan value: set a provider's monthly plan price in display settings to compare it with this month's API-equivalent cost, projection and cache savings, per provider and on the Overview.
- Add run-out forecasts: when a quota window is on track to run out before its reset at the current rate, quota bars and `usagestat statusline` say when. A new `quota_forecast` hook event alerts once per window.
- Add cost per git branch: `usagestat project --branches`, a `branches` local usage report, and expandable project rows in the Projects tab. Claude Code and Codex record the branch (cache v13, rebuilt once).
- Projects from agent worktrees (T3 `~/.t3/worktrees/<repo>/t3-…`, Claude Code `<repo>/.claude/worktrees/…`, Codex `~/.codex/worktrees/<id>/<repo>`) are labelled with their repository (cache v11, rebuilt once).
- Add optional combined overview cards for several accounts of the same provider, and an Image button that saves a local PNG summary.
- Collection settings saves now edit `config.toml` in place, keeping key order and comments.
- Fix the systemd unit's `EnvironmentFile=` line, which was quoted and so ignored `daemon.env`. Re-running `daemon enable` repairs existing units.
- Rebrand the dashboard as usagestat. Tooltip explanations appear only in pinned details. Collection settings explain when the backend runs without polling or on a non-local address.

See [release notes](docs/releases/v2.3.0.md).

## 2.2.0 - 2026-11-15

- Add GPT-6.1 Sol API-equivalent cost estimates, including cached input, cache writes and long-context rates. Saved local transcripts are reparsed for the new pricing; retained billing charges remain authoritative.
- Add opt-in SiliconFlow, Tripo, OpenDesign and Google Developer providers (124 total). Update Helmcode, TypeSafe, MuseAI, Trae and WorkBuddy readbacks from reviewed upstream sources.
- Improve Cursor encrypted-token handling and read-only stable CLI fallback, Perplexity manual-session readback, Codex monthly windows, Kimi monthly breakdowns, OpenCode credentials and Synthetic weekly plans. JetBrains gains modern quota parsing and optional bounded local observations.

See [release notes](docs/releases/v2.2.0.md) and the [publication record](docs/releases/v2.2.0-publication.md).

## 2.1.0 - 2026-10-26

- Add 20 opt-in providers (120 total): Abliteration, Aerostack, Chatbox, Claude Platform, Cosmic, Exa, Fal, Flora, FutureAGI, Langdock, Linkup, Munito, Parallel, Sail Research, Sofya, Soniox, Tavily, TinyAPI, Trae CN and X API. Vercel personal budgets and QwenCloud Team Token Plan become explicit alternative sources.
- Correct quota and accounting for OpenCode Go credential migration, Claude reset grants and cloud credits, Cursor unified weekly pools and native billing events, Antigravity merged hosts and Argon weekly pool, Ollama balances, Kimi ratio pools and xAI invoice previews.
- Carry upstream transport and parsing fixes for existing bundled definitions, including Helmcode routing, Hyper source labels and WorkBuddy billing cycles.

See [release notes](docs/releases/v2.1.0.md) and the [publication record](docs/releases/v2.1.0-publication.md).

## 2.0.2 - 2026-10-04

- Adopt Antigravity's Hub request identity, keeping permission denial distinct from expired authentication.
- Read Codex `token_usage_record` request ledgers with thread/session ownership, explicit child-history boundaries, replay and legacy-mirror deduplication. Retain identity through private cache reloads and migrate older cache summaries.
- Support OpenCode 2 current credentials and logout semantics, assistant/completed-compaction history, and migrated message deduplication in explicitly selected databases.
- Prefer Cursor Teams model pools over legacy dollar meters and omit unknown combined usage. Preserve dollar readback when zero pool fields are placeholders.
- Update Muse Code browser fallback to bind blank-email sessions through matching team membership.
- Add opt-in Muse (`muse.ai`), LithosAI and WorkBuddy web providers with provider-owned icons. Keep prepaid balances, weekly quotas, top-ups and spend distinct; bound action discovery and scope cookie/CSRF injection to declared provider routes.
- Expose provider-specific setup requirements in dashboard Collection settings, including WorkBuddy's cookie-owning Browser User-Agent. Live sessions remain unqualified; TypeSafe Cloudflare protection remains explicit.

See [release notes](docs/releases/v2.0.2.md) and the [publication record](docs/releases/v2.0.2-publication.md) for qualification and channel availability.

## 2.0.1 - 2026-09-30

- Add independent dashboard Display settings for provider visibility, ordering, names, trackers, layout, themes, thresholds and notifications. Choose only a provider's own related logo variants, with separate monochrome/colour controls and optional uploaded logos.
- Add local dashboard Collection setup for provider sources, write-only credentials, polling and explicit account instances. Private setup-file proof and a per-launch session protect the loopback-only configuration API; saved changes reload without restarting the daemon.
- Add hover, keyboard-focus and tap details throughout quota, history, token and cost charts, preserving missing, stale and failed observations and separating API-equivalent costs from subscription charges.
- Improve upload buttons, checkboxes, keyboard focus and responsive settings layouts.
- Distinguish Antigravity quota/license denial from expired authentication, retaining explicit unavailable states.
- Ship stable COPR feed configuration, an optional daily RPM update timer and a package transaction hook that refreshes already-running RPM-owned services while preserving their configuration and enablement.
- Preserve the native `/v1` readback and `agenticdriver.usage.v2` ingestion contracts, account bindings, credential scopes and shared provider-icons pin.

See [release notes](docs/releases/v2.0.1.md) and the [publication record](docs/releases/v2.0.1-publication.md) for qualification and channel availability.

## 2.0.0-alpha.4 - 2026-09-28

- Bundle 97 providers with 20 new opt-in integrations, updated quota and account handling, and shared provider icons with their license notices.
- Refresh the dashboard with cost and token breakdowns, cache savings, model history, source comparisons, smooth quota charts, stacked cost charts, and responsive views.
- Persist daily model usage with pricing and source provenance; prefer billing, then deduplicated local transcripts, then ccusage. Keep missing prices and partial data explicit.
- Cache local usage scans and serve compact, bounded history responses to keep large histories responsive.
- Add standalone Claude quota observations and a bounded no-prompt CLI fallback that respects account isolation and throttling.
- Fix Antigravity access-token selection, refresh, account-specific caches, and personal-account quota fallback.
- Preserve actionable credential states in native JavaScript errors and reject local reports for missing explicitly selected profiles.
- Add Prometheus health/quota metrics and update the T3 hub adapter for current cached-usage contracts.
- Build the daemon before HTTP integration tests and verify provider inventories and shared icons before release packaging.

See [release notes](docs/releases/v2.0.0-alpha.4.md) for downloads and qualification limits.

## 2.0.0-alpha.1 - 2026-09-07

- Introduce the Windows/macOS backend alpha with native archives, per-user services, portable paths, credential/helper adapters, provider discovery and capability diagnostics.
- Add transactional Windows installation and retained-version daemon relocation, including native rollback, interrupted recovery and external-lock fixtures.
- Stage and test native npm packages on Linux, macOS and Windows; public npm publication remains pending.
- Keep this unsigned alpha on GitHub Releases while desktop/account/minimum-OS/bar/signing qualification remains open. See [alpha installation](docs/alpha.md) and [release notes](docs/releases/v2.0.0-alpha.1.md).

- Add provider plugin entries for Abacus AI, Alibaba, Alibaba Token Plan, AWS Bedrock, Azure OpenAI, Command Code, Deepgram, Droid, ElevenLabs, Grok, GroqCloud, LLM Proxy, Manus, Moonshot, OpenCode, StepFun, T3 Chat, Vertex AI, and Xiaomi MiMo.
- Back `usagestat cost` and Codex/Claude cost rows with pinned ccusage runners, including total-token columns and 30-day cost summaries.
- Add a local beta workflow: `scripts/install-dev.sh` installs `usagestat-dev` and synced dev plugins under `~/.local`.
- Harden ccusage runner discovery and cleanup by resolving NVM default aliases and killing Unix child process groups on timeout.
- Move provider icon and link metadata into plugin manifests/list output, with absolute monochrome SVG paths and separate color SVG paths where available.
- Add T3 Chat web probing with configured cookie/cURL-header support for Vercel-protected sessions.
- Include T3 Chat full-cURL fallback instructions in cookie-import results.

## 1.0.2 - 2026-05-18

- Add `usagestat test https` to smoke-test the same HTTPS path used by provider plugins.
- Install provider plugins in distro packages and release tarballs.
- Discover installed provider plugins from the binary install prefix.
- Move dev-only providers (`mock`, `host-smoke`) out of the production plugin bundle.
- Add a copyable custom provider plugin template.

## 1.0.1 - 2026-05-18

- Fix HTTPS provider probes by installing the rustls ring crypto provider before plugin HTTP requests.

## 1.0.0 - 2026-05-17

- Initial UsageStat release.
- Publish Linux release binaries and tarballs.
- Add package metadata for COPR, AUR, Homebrew, and Launchpad PPA.
