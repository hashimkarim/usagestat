const { test } = require("node:test");
const assert = require("node:assert/strict");
const { load, lines, metric, response, NOW } = require("./provider-sync-harness.cjs");

test("Ollama parses monthly included credits, plan, and reset without borrowing weekly values", () => {
  const app = load("ollama", { env: { OLLAMA_COOKIE: "cookie-token", OLLAMA_API_KEY: "api-key" },
    request: () => response('<span>Included usage</span><span class="plan">pro</span\n><span>Monthly usage</span><span>$7.50 of $60 used</span><div data-time="2026-09-30T15:14:29Z"></div><span>Weekly usage</span><span>42% used</span>') });
  const result = app.probe();
  assert.equal(result.source, "web");
  assert.equal(result.plan, "pro");
  assert.equal(metric(result, "Monthly credits").used, 12.5);
  assert.equal(metric(result, "Monthly credits").resetsAt, "2026-09-30T15:14:29.000Z");
  assert.equal(metric(result, "Weekly").used, 42);
  assert.equal(app.requests.length, 1);
  assert.equal(app.requests[0].headers.Cookie, "__Secure-session=cookie-token");
});

test("Ollama keeps fractional usage and does not borrow a later window", () => {
  const app = load("ollama", { provider: { cookieHeader: "Cookie: __Secure-session=token" },
    request: () => response("<span>Monthly usage</span><span>$1,25 of $60 used</span><span>Session usage</span><span>0.5% used</span><span>Weekly usage</span><span>17% used</span>") });
  const result = app.probe();
  assert.equal(metric(result, "Monthly credits"), undefined);
  assert.equal(metric(result, "Session").used, 0.5);
  assert.equal(metric(result, "Weekly").used, 17);
});

test("Ollama API-only access never invents quota meters", () => {
  const app = load("ollama", { source: "api", provider: { apiKey: "key" },
    request: (req) => req.method === "POST" ? response({}, 400) : response({ models: [{ name: "model-cloud" }] }) });
  const result = app.probe();
  assert.equal(result.source, "api");
  assert.equal(metric(result, "Cloud Models").value, "1 available");
  assert.ok(!lines(result).some((line) => line.type === "progress"));
});

test("OpenRouter key cap uses the current window and keeps management credentials on OpenRouter", () => {
  const row = { date: "2026-09-04", model: "model", usage: 1.25, prompt_tokens: 100, completion_tokens: 20, reasoning_tokens: 5, requests: 1 };
  const app = load("openrouter", { provider: { apiKey: "ordinary", settings: { baseUrl: "https://proxy.example/api/v1", managementApiKey: "management" } },
    request: (req) => {
      if (req.url.endsWith("/credits")) return response({ data: { total_credits: 100, total_usage: 10 } });
      if (req.url.endsWith("/key")) return response({ data: { limit: 20, limit_remaining: 19.75, usage: 999 } });
      return response({ data: [row] });
    } });
  const result = app.probe();
  assert.equal(metric(result, "Key Cap").used, 0.25);
  assert.equal(metric(result, "Cost").value, "$1.25");
  assert.equal(app.ingested[0].daily.length, 1);
  assert.equal(app.ingested[0].daily[0].costUsd, 1.25);
  for (const req of app.requests.filter((item) => item.headers.Authorization === "Bearer management")) {
    assert.equal(new URL(req.url).hostname, "openrouter.ai");
  }
});

test("OpenRouter conflicting history leaves quota intact and does not overwrite history", () => {
  let count = 0;
  const app = load("openrouter", { provider: { apiKey: "key", settings: { managementApiKey: "management" } },
    request: (req) => {
      if (req.url.endsWith("/credits")) return response({ data: { total_credits: 10, total_usage: 1 } });
      if (req.url.endsWith("/key")) return response({});
      return response({ data: [{ date: "2026-09-04", model: "m", usage: ++count, prompt_tokens: 1, completion_tokens: 1, requests: 1 }] });
    } });
  const result = app.probe();
  assert.equal(metric(result, "Credits").used, 1);
  assert.equal(metric(result, "Cost"), undefined);
  assert.equal(app.ingested.length, 0);
});

test("Z.ai handles credit plans, regional balance, team headers, and implausible resets", () => {
  const app = load("zai", { provider: { apiKey: "cn-key", region: "bigmodel-cn", workspaceId: "project", settings: { usageScope: "team", organization: "org" } },
    request: (req) => {
      if (req.url.includes("/quota/")) return response({ success: true, code: 200, data: { planName: "Pro", limits: [
        { type: "CREDIT_LIMIT", unit: 3, number: 5, percentage: "0.5", nextResetTime: Date.parse(NOW) + 10 * 3600000 },
        { type: "CREDIT_LIMIT", unit: 6, number: 1, percentage: 25, nextResetTime: Date.parse("2026-09-10T12:00:00Z") },
      ] } });
      return response({ success: true, code: 200, data: { availableBalance: 0, balance: 40 } });
    } });
  const result = app.probe();
  assert.equal(metric(result, "Session").used, 0.5);
  assert.equal(metric(result, "Session").resetsAt, undefined);
  assert.equal(metric(result, "Weekly").used, 25);
  assert.equal(metric(result, "Balance").value, "CNY 0.00");
  assert.equal(app.requests[0].headers["Bigmodel-Organization"], "org");
  assert.equal(app.requests[0].headers["Bigmodel-Project"], "project");
  assert.ok(app.requests[0].url.endsWith("?type=2"));
});

test("Z.ai rejects booleans instead of reporting false zero usage", () => {
  const app = load("zai", { env: { ZAI_API_KEY: "key" },
    request: () => response({ limits: [{ type: "CREDIT_LIMIT", unit: 3, percentage: false }] }) });
  assert.throws(app.probe, /invalid/i);
});

test("OpenCode Go uses official meters before local estimates and preserves 1%", () => {
  const app = load("opencode-go", { provider: { apiKey: "go-key" }, sqlite: () => { throw new Error("local history must not replace official quotas"); },
    request: () => response({ usage: { rolling: { percent: 1, resetsAt: "2026-09-05T15:00:00Z" }, weekly: { percent: 0.5 }, monthly: { percent: 23 } } }) });
  const result = app.probe();
  assert.equal(app.requests[0].url, "https://opencode.ai/zen/go/v1/usage");
  assert.equal(result.source, "api");
  assert.equal(metric(result, "Session").used, 1);
  assert.equal(metric(result, "Weekly").used, 0.5);
});

test("OpenCode Go rejected keys do not silently fall back to local estimates", () => {
  const app = load("opencode-go", { provider: { apiKey: "key" }, request: () => response({ error: { type: "EntitlementError" } }, 403) });
  assert.throws(app.probe, /No OpenCode Go subscription/);
});

test("ClinePass decodes all three windows without rescaling fractional percentages", () => {
  const app = load("clinepass", { provider: { apiKey: "key" }, request: () => response({ success: true, data: { limits: [
    { type: "five_hour", percentUsed: 0.5, resetsAt: "2026-09-05T15:00:00Z" },
    { type: "weekly", percentUsed: 20 }, { type: "monthly", percentUsed: 30 },
  ] } }) });
  assert.deepEqual(lines(app.probe()).map((line) => line.used), [0.5, 20, 30]);
});

test("ZenMux decodes fractional quota ratios and a confirmed zero PAYG balance", () => {
  const app = load("zenmux", { provider: { apiKey: "key" }, request: (req) => req.url.endsWith("/payg/balance")
    ? response({ success: true, data: { currency: "USD", total_credits: 0 } })
    : response({ success: true, data: { plan: { tier: "pro" }, quota_5_hour: { usage_percentage: 0.005 }, quota_7_day: { usage_percentage: 0.25 } } }) });
  const result = app.probe();
  assert.equal(metric(result, "5-hour quota").used, 0.5);
  assert.equal(metric(result, "PAYG Balance").value, "$0.00");
});

test("DeepInfra accounts for prepaid balance, monthly cents, and suspension", () => {
  const app = load("deepinfra", { provider: { apiKey: "key" }, request: (req) => req.url.includes("checklist")
    ? response({ stripe_balance: -50, recent: 5, limit: 100, suspended: true })
    : response({ months: [{ total_cost: 500 }] }) });
  const result = app.probe();
  assert.equal(metric(result, "Billing Cycle").used, 5);
  assert.ok(metric(result, "Billing Cycle").detail.includes("$45.00 available"));
  assert.equal(metric(result, "Month Cost").value, "$5.00");
  assert.equal(metric(result, "Status").text, "Suspended");
});

test("ai& paginates request logs and marks incomplete pages", () => {
  const app = load("aiand", { provider: { apiKey: "key" }, request: (req, index) => response({
    data: [{ cost: "1.25", currency: "USD" }], has_more: true, next_after: index === 1 ? "cursor" : null, next_after_id: "id",
  }) });
  const result = app.probe();
  assert.equal(metric(result, "Cost").value, "$2.50");
  assert.match(metric(result, "Cost").subtitle, /partial/);
  assert.equal(app.requests.length, 2);
  assert.ok(app.requests[1].url.includes("after_id=id"));
});

test("xAI displays a numeric zero balance and ingests daily history", () => {
  const app = load("xai", { provider: { apiKey: "key", workspaceId: "team" }, request: (req) => req.method === "GET"
    ? response({ total: { val: 0 } })
    : response({ timeSeries: [{ dataPoints: [{ timestamp: "2026-09-04", values: [2.5] }] }] }) });
  const result = app.probe();
  assert.equal(metric(result, "Balance").value, "$0.00");
  assert.equal(metric(result, "Cost").value, "$2.50");
  assert.equal(app.ingested[0].daily[0].costUsd, 2.5);
});

test("sub2api supports local gateways and all rolling quota windows", () => {
  const app = load("sub2api", { provider: { apiKey: "key", settings: { baseUrl: "http://[::1]:8080/v1" } },
    request: () => response({ subscription: { daily_usage_usd: 1, daily_limit_usd: 5, weekly_usage_usd: 10, weekly_limit_usd: 100, monthly_usage_usd: 20, monthly_limit_usd: 500 },
      balance: 3, rate_limits: [{ window: "5h", used: 2, limit: 10, reset_at: "2026-09-05T17:00:00Z" }] }) });
  const result = app.probe();
  assert.ok(app.requests[0].url.startsWith("http://[::1]:8080/v1/usage?"));
  assert.equal(metric(result, "Daily").used, 20);
  assert.equal(metric(result, "5 hour limit").used, 20);
});

test("Qoder merges shared and personal credit pools", () => {
  const app = load("qoder", { provider: { cookieHeader: "session=token" }, request: () => response({ data: {
    totalQuota: { quotaSummary: { usedValue: 10, limitValue: 100 } },
    sharedQuota: { quotaSummary: { usedValue: 20, limitValue: 100 } },
  } }) });
  assert.equal(metric(app.probe(), "Credits").used, 15);
});

test("Sakana preserves sub-1% usage and separate resets", () => {
  const app = load("sakana", { provider: { cookieHeader: "session=token" },
    request: () => response("<div>5-hour 0.5% used</div><div>Weekly 21% used September 10, 2026 at 12:00 PM</div>") });
  const result = app.probe();
  assert.equal(metric(result, "5-hour").used, 0.5);
  assert.equal(metric(result, "5-hour").resetsAt, undefined);
  assert.equal(metric(result, "Weekly").used, 21);
});

test("LongCat prefers active token packs and retains fuel packs", () => {
  const app = load("longcat", { provider: { cookieHeader: "session=token" }, request: (req) => {
    if (req.url.includes("summary")) return response({ data: { currentLot: { status: "ACTIVE", totalToken: 1000, consumedToken: 250 } } });
    if (req.url.includes("fuel") || req.url.includes("Fuel")) return response({ data: { packages: [{ totalToken: 100, availableToken: 40 }] } });
    return response({ data: { name: "Account" } });
  } });
  const result = app.probe();
  assert.equal(metric(result, "Quota").used, 25);
  assert.equal(metric(result, "Fuel Pack").used, 60);
});

test("Notion reads nested workspace records and rolling/billing windows", () => {
  const app = load("notion", { provider: { cookieHeader: "token", workspaceId: "workspace" }, request: (req) => req.url.endsWith("/getSpaces")
    ? response({ user: { notion_user: { user: { value: { id: "user" } } }, space: { workspace: { value: { value: { id: "workspace", name: "Work", subscription_tier: "business" } } } } } })
    : response({ window: { used: 5, limit: 100, window: "5h" }, resetsInSeconds: 3600,
      billingPeriodWindow: { used: 20, limit: 100, periodEndMs: Date.parse("2026-09-30T00:00:00Z") } }) });
  const result = app.probe();
  assert.equal(metric(result, "Rolling").used, 5);
  assert.equal(metric(result, "Monthly").used, 20);
  assert.equal(app.requests[0].headers.Cookie, "token_v2=token");
});

test("Notion never silently substitutes a different explicitly selected workspace", () => {
  const app = load("notion", { provider: { cookieHeader: "token", workspaceId: "missing" }, request: () =>
    response({ user: { space: { different: { value: { id: "different", subscription_tier: "business" } } } } }) });
  assert.throws(app.probe, /workspace/i);
  assert.equal(app.requests.length, 1);
});

test("CodeBuddy accepts precise credit fields and writes a local cache", () => {
  const app = load("codebuddy", { provider: { cookieHeader: "session=token" }, request: () => response({ code: 0, data: { Response: { Data: {
    Accounts: [{ CapacitySizePrecise: "100.5", CapacityUsedPrecise: "20.1", CapacityRemainPrecise: "80.4" }],
  } } } }) });
  assert.ok(Math.abs(metric(app.probe(), "Credits").used - 20) < 0.00001);
  assert.ok(app.files.has("/test/home/.codebuddy/cb_credits.json"));
});

test("ZoomMate scopes captured cookies to their host during fallback", () => {
  const app = load("zoommate", { provider: { cookieHeader: "curl 'https://zoommate.zoom.us/ai-computer/api/v1/credits/status' -H 'Authorization: Bearer token' -H 'Cookie: host_session=secret'" },
    request: (req, index) => index === 1 ? response({}, 503) : response({ data: { credit_status: { budget_cap: 100, used_credit: 20, cycle_start_date: Date.parse("2026-09-01"), cycle_end_date: Date.parse("2026-10-01") } } }) });
  assert.equal(metric(app.probe(), "Credits").used, 20);
  assert.equal(app.requests[0].headers.Cookie, "host_session=secret");
  assert.equal(app.requests[1].headers.Cookie, undefined);
});

test("IBM Bob loads a regional team budget with API-key authentication", () => {
  const app = load("ibmbob", { provider: { apiKey: "key" }, request: (req) => req.url.endsWith("/profile")
    ? response({ instances: [{ instance_id: "instance", instance_name: "Work", user_id: "user", region_domain: "us-east.bob.ibm.com", refresh_at: "2026-10-01T00:00:00Z", teams: [{ id: "team", name: "Team" }] }] })
    : response({ usage: 20, budget_limit: 100 }) });
  assert.equal(metric(app.probe(), "Work / Team").used, 20);
  assert.equal(app.requests[1].headers.Authorization, "Apikey key");
  assert.equal(app.requests[1].headers["x-team-id"], "team");
});

test("IBM Bob rejects foreign regional hosts before sending credentials", () => {
  const app = load("ibmbob", { provider: { apiKey: "key" }, request: () =>
    response({ instances: [{ user_id: "u", region_domain: "bob.ibm.com.evil.test", teams: [{ id: "team" }] }] }) });
  assert.throws(app.probe, /untrusted/);
  assert.equal(app.requests.length, 1);
});

test("ClawRouter converts microdollars and handles a December budget reset", () => {
  const app = load("clawrouter", { provider: { apiKey: "key" }, request: () => response({
    budget: { configured: true, limitMicros: 10000000, spentMicros: 1250000, windowKey: "month:2026-12" },
    usage: { summary: { requestCount: 2, successCount: 2, errorCount: 0, inputTokens: 10, outputTokens: 5, totalTokens: 15, actualCostMicros: 1250000 }, providers: [] },
  }) });
  const result = app.probe();
  assert.equal(metric(result, "Monthly budget").used, 1.25);
  assert.equal(metric(result, "Monthly budget").resetsAt, "2027-01-01T00:00:00.000Z");
});

test("Wayfinder never labels unpriced savings as dollars or invents a quota", () => {
  const app = load("wayfinder", { request: (req) => {
    if (req.url.endsWith("/healthz")) return response({ status: "ok", offline: false });
    if (req.url.endsWith("/router/models")) return response({ models: [{ name: "local" }], dry_run: false });
    if (req.url.endsWith("/metrics")) return response("wayfinder_router_decision_latency_seconds_sum 0.04\nwayfinder_router_decision_latency_seconds_count 2\n");
    return response({ priced: false, requests: 2, tokens: 30, saved: 3, saved_pct: 20, by_route: { local: { requests: 2 } } });
  } });
  const result = app.probe();
  assert.equal(metric(result, "Saved").value, "20.0%");
  assert.equal(metric(result, "Avg decision").value, "20.0 ms");
  assert.ok(!lines(result).some((line) => line.type === "progress"));
});

for (const id of ["aiand", "deepinfra", "clinepass", "zenmux", "xai", "sub2api", "qoder", "sakana", "notion", "longcat", "codebuddy", "ibmbob", "clawrouter", "ollama", "opencode-go"]) {
  test(id + " fails clearly on rejected credentials", () => {
    const app = load(id, { source: ["ollama", "opencode-go"].includes(id) ? "api" : "auto",
      provider: { apiKey: "key", cookieHeader: "session=token", workspaceId: id === "sub2api" ? "https://gateway.example" : "team" },
      request: () => response({}, 401) });
    assert.throws(app.probe);
  });
}
