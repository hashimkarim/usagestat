# Claude quota sources

UsageStat works independently of T3 Code and Claude Desktop. It does not need a
wrapper, status-line hook, or changes to another application's settings.

In `auto` mode, the signed-in Claude Code **file-backed profile** can supply:

1. Its recent `cachedUsageUtilization` observation, read without modifying the
   Claude configuration. UsageStat verifies the account, organization, profile,
   and credential fingerprint. Observations older than five minutes, future
   timestamps, invalid percentages, and expired windows are not fresh quota.
2. The existing OAuth usage endpoint and configured-cookie web fallback.
3. When OAuth fails without an active cooldown, a standalone Claude CLI
   `initialize` / `get_usage` control dialogue. This
   sends no user prompt and performs no inference. Tools, hooks, MCP servers,
   session persistence, project settings, and transcript behavior scans are
   disabled. The process runs in an empty temporary directory, has a 12-second
   timeout and a 1 MiB output limit, and is terminated with its descendants.

CLI checks are throttled across UsageStat processes. An OAuth `Retry-After`
deadline suppresses new CLI requests too: the CLI can use the same endpoint,
so starting it repeatedly is not a way around server throttling. Fresh local
observations remain usable during that cooldown without clearing it.

Claude may return a cached quota when its own request fails. UsageStat retains
that cache's observation time instead of presenting it as a new successful
fetch. Cost/token history remains independent; it is never converted into a
subscription quota estimate. Model-specific quota rows remain opt-in.

The CLI dialogue currently runs on Linux/macOS with a file-backed login.
Windows can read fresh file-backed cache observations but retains OAuth/web for
network refresh. Keychain-only macOS logins retain their existing OAuth/web paths.
Named UsageStat accounts and environment-based credential/provider overrides
never borrow a different ambient Claude login. CLI control APIs are
experimental; unsupported versions fall back to the existing sources in auto
mode, without accepting missing quota as zero.

To inspect only this standalone source:

```sh
usagestat usage claude --source cli --json
```

`source` is `cli-cache` for a Claude-owned observation or `cli` for a validated
control response. `fetchedAt` is the observation's timestamp, not the time the
Bar happened to refresh. Explicit `oauth`, `web`, `api`, and `local` modes
retain their previous behavior.
