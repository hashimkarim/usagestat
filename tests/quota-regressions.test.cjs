const { test } = require("node:test");
const assert = require("node:assert/strict");
const { load, lines, metric, response, NOW } = require("./provider-sync-harness.cjs");

const claudeCredentials = { accessToken: "token", expiresAt: Date.parse(NOW) + 3600000 };
const claudeUsage = { five_hour: { utilization: 10 }, seven_day: { utilization: 25 } };

test("Copilot keeps personal credits distinct from organization billing and suppresses placeholder quotas", () => {
  const app = load("copilot", { env: { GH_TOKEN: "ghp_test" }, provider: { workspaceId: "work" }, request: (req) => {
    if (req.url.endsWith("/copilot_internal/user")) return response({ copilot_plan: "business", token_based_billing: true,
      quota_snapshots: { premium_interactions: { entitlement: 0, percent_remaining: 100, credits_used: 123.5, overage_permitted: true }, chat: { entitlement: -1, percent_remaining: 100 } } });
    return response({ usageItems: [{ product: "copilot", unitType: "ai-credits", grossQuantity: 999, netAmount: 2 },
      { product: "copilot", unitType: "seats", grossQuantity: 10, netAmount: 1000 }] });
  } });
  const result = app.probe();
  assert.equal(metric(result, "Credits").value, "123.5 credits");
  assert.equal(metric(result, "Org Credits").value, "999 credits");
  assert.equal(metric(result, "Org Spend").value, "$2.00");
  assert.ok(!lines(result).some((line) => line.type === "progress"));
  assert.equal(metric(result, "Extra Usage"), undefined);
});

test("Copilot supports credit quotas, suppresses unlimited rows, and uses calendar months", () => {
  const app = load("copilot", { env: { GH_TOKEN: "ghp_test" }, request: () => response({ copilot_plan: "pro", quota_reset_date: "2026-08-01T00:00:00Z",
    quota_snapshots: { premium_interactions: { entitlement: 100, remaining: 75, overage_permitted: true, overage_count: 0 }, chat: { entitlement: -1 }, completions: { unlimited: true } } }) });
  const result = app.probe();
  assert.equal(metric(result, "Credits").used, 25);
  assert.equal(metric(result, "Credits").periodDurationMs, 31 * 86400000);
  assert.equal(metric(result, "Extra Usage").value, "0");
  assert.equal(metric(result, "Chat"), undefined);
});

test("Factory API-key mode needs no CLI login and auto mode can recover with a configured cookie", () => {
  const usage = { usage: { standard: { totalAllowance: 20000000, orgTotalTokensUsed: 5000000 } } };
  const api = load("factory", { source: "api", provider: { apiKey: "fk_test" }, request: () => response(usage) });
  assert.equal(metric(api.probe(), "Standard").used, 5000000);
  assert.equal(api.requests[0].headers.Authorization, "Bearer fk_test");
  const auto = load("factory", { provider: { apiKey: "bad", cookieHeader: "session=valid" }, request: (req) => req.headers.Cookie ? response(usage) : response({}, 401) });
  assert.equal(auto.probe().source, "web");
  assert.equal(auto.requests[1].headers.Authorization, undefined);
  assert.throws(load("factory", { source: "api", provider: { apiKey: "bad", cookieHeader: "session=valid" }, request: () => response({}, 401) }).probe, /rejected/);
});

test("Sakana weekly-only quotas and PAYG balances survive optional failures", () => {
  const app = load("sakana", { provider: { cookieHeader: "session=token" }, request: (req) => req.url.includes("payAsYouGo")
    ? response('<h2>Credit balance</h2><p class="tabular-nums">$0.00</p><h2>Usage</h2><span>Total: <!-- -->$1.25</span>')
    : response("<div>Weekly 20% used</div>") });
  const result = app.probe();
  assert.equal(metric(result, "Weekly").used, 20);
  assert.equal(metric(result, "PAYG Balance").value, "$0.00");
  assert.equal(metric(result, "PAYG Usage").value, "$1.25");
});

function grok(config) {
  return load("grok", { env: { GROK_HOME: "/test/grok" }, files: { "/test/grok/auth.json": JSON.stringify({ session: { key: "token" } }) },
    request: (req) => req.url.includes("/billing") ? response({ config }) : response({ subscription_tier_display: "SuperGrok" }) });
}

test("Grok uses credits-format weekly periods and restores omitted proto3 zero usage", () => {
  const app = grok({ currentPeriod: { type: "USAGE_PERIOD_TYPE_WEEKLY", start: "2026-09-03T00:00:00Z", end: "2026-09-10T00:00:00Z" } });
  const result = app.probe();
  assert.ok(app.requests[0].url.endsWith("?format=credits"));
  assert.equal(metric(result, "Weekly limit").used, 0);
  assert.equal(metric(result, "Weekly limit").periodDurationMs, 7 * 86400000);
});

test("Grok rejects incomplete, future, and malformed periods instead of showing zero", () => {
  for (const config of [
    { currentPeriod: {} },
    { currentPeriod: { type: "USAGE_PERIOD_TYPE_WEEKLY", start: "2026-09-06T00:00:00Z", end: "2026-09-13T00:00:00Z" } },
    { creditUsagePercent: null, currentPeriod: { type: "USAGE_PERIOD_TYPE_WEEKLY", start: "2026-09-03T00:00:00Z", end: "2026-09-10T00:00:00Z" } },
  ]) assert.throws(grok(config).probe);
});

test("ZoomMate represents unlimited credits without a false zero meter", () => {
  const result = load("zoommate", { provider: { cookieHeader: "Bearer token" }, request: () => response({ data: { credit_status: { is_unlimited: true } } }) }).probe();
  assert.equal(metric(result, "Credits").text, "Unlimited");
  assert.ok(!lines(result).some((line) => line.type === "progress"));
});

test("Claude prefers active limits[] windows and hides all model quotas by default", () => {
  const app = load("claude", { credentials: claudeCredentials, request: () => response({ ...claudeUsage,
    seven_day_sonnet: { utilization: 40 }, seven_day_opus: { utilization: 80 }, limits: [
      { kind: "session", percent: 90, is_active: false },
      { kind: "session", percent: 0.5, is_active: true, resets_at: "2026-09-05T15:00:00Z" },
      { kind: "weekly_all", percent: 30, is_active: true },
      { kind: "weekly_scoped", percent: 75, scope: { model: { display_name: "Fable" } } },
    ] }) });
  const result = app.probe();
  assert.equal(metric(result, "Session").used, 0.5);
  assert.equal(metric(result, "Weekly").used, 30);
  assert.deepEqual(lines(result).map((line) => line.label), ["Session", "Weekly"]);
  assert.equal(result.source, "oauth");
});

test("Claude model quotas remain explicitly opt-in", () => {
  const app = load("claude", { credentials: claudeCredentials, provider: { settings: { showModelQuotas: true } },
    request: () => response({ ...claudeUsage, seven_day_sonnet: { utilization: 40 } }) });
  assert.equal(metric(app.probe(), "Sonnet").used, 40);
});

test("Claude web fallback keeps its real source and distinguishes Max tiers", () => {
  const app = load("claude", { credentials: claudeCredentials, provider: { cookieHeader: "sessionKey=web-token" }, request: (req) => {
    if (req.url.includes("/oauth/usage")) return { ...response({}, 429), headers: { "retry-after": "900" } };
    if (req.url.endsWith("/organizations")) return response([{ uuid: "org" }]);
    if (req.url.endsWith("/account")) return response({ rate_limit_tier: "claude_max_20" });
    return response(claudeUsage);
  } });
  const result = app.probe();
  assert.equal(result.source, "web");
  assert.equal(result.plan, "Claude Max 20x");
  assert.equal(metric(result, "Session").used, 10);
  assert.equal(metric(result, "Status"), undefined);
});

test("Claude malformed web responses cannot poison the quota cache", () => {
  const app = load("claude", { source: "web", provider: { cookieHeader: "sessionKey=token" }, request: (req) => req.url.endsWith("/organizations")
    ? response([{ uuid: "org" }]) : response({}) });
  assert.throws(app.probe, /Web usage response invalid/);
  assert.equal(app.files.size, 0);
});

test("OpenCode Go saves daily local costs without replacing official quotas", () => {
  const app = load("opencode-go", { provider: { apiKey: "key" },
    sqlite: () => JSON.stringify([{ createdMs: Date.parse("2026-09-04T12:00:00Z"), cost: 1.25 }]),
    request: () => response({ usage: { rolling: { percent: 20 }, weekly: { percent: 30 } } }) });
  const result = app.probe();
  assert.equal(result.source, "api");
  assert.equal(metric(result, "Session").used, 20);
  assert.equal(app.ingested[0].daily[0].costUsd, 1.25);
  assert.equal(metric(result, "Local Cost").value, "$1.25");
  assert.equal(metric(result, "Monthly"), undefined);
});

test("OpenCode Go supports relative resets and rejects malformed optional windows", () => {
  const result = load("opencode-go", { provider: { apiKey: "key" }, request: () => response({ usage: {
    rolling: { percent: 0.5, resetInSec: 3600 },
  } }) }).probe();
  assert.equal(metric(result, "Session").resetsAt, "2026-09-05T13:00:00.000Z");
  assert.equal(metric(result, "Weekly"), undefined);
  for (const usage of [{ weekly: { percent: 0 } }, { rolling: { percent: 20 }, weekly: { percent: null } }]) {
    assert.throws(load("opencode-go", { provider: { apiKey: "key" }, request: () => response({ usage }) }).probe, /invalid quota/);
  }
});

test("OpenCode Go local mode labels estimates and skips web enrichment", () => {
  const app = load("opencode-go", { source: "local", provider: { apiKey: "key", cookieHeader: "session=token" },
    sqlite: (db, sql) => JSON.stringify(sql.includes("present") ? [{ present: 1 }] : [{ createdMs: Date.parse(NOW) - 1000, cost: 1 }]),
    request: () => { throw new Error("local mode must not request web data"); } });
  const result = app.probe();
  assert.equal(result.source, "local-estimate");
  assert.match(metric(result, "Session").detail, /Estimated/);
  assert.equal(app.requests.length, 0);
});

test("Claude persists successful quotas, honors Retry-After, and keeps the original timestamp", () => {
  const first = load("claude", { credentials: claudeCredentials, request: () => response(claudeUsage) });
  first.probe();
  const second = load("claude", { credentials: claudeCredentials, now: "2026-09-05T12:06:00Z", files: Object.fromEntries(first.files),
    request: () => ({ ...response({}, 429), headers: { "retry-after": "900" } }) });
  const limited = second.probe();
  assert.equal(metric(limited, "Session").used, 10);
  assert.equal(limited.fetchedAt, "2026-09-05T12:00:00.000Z");
  assert.equal(limited.source, "cached");
  const third = load("claude", { credentials: claudeCredentials, now: "2026-09-05T12:07:00Z", files: Object.fromEntries(second.files),
    request: () => { throw new Error("must respect cooldown"); } });
  assert.equal(metric(third.probe(), "Weekly").used, 25);
  assert.equal(third.requests.length, 0);
});

test("Claude never reuses quotas after switching credentials", () => {
  const first = load("claude", { credentials: claudeCredentials, request: () => response(claudeUsage) });
  first.probe();
  const second = load("claude", { credentials: { ...claudeCredentials, accessToken: "different" }, files: Object.fromEntries(first.files),
    request: () => response({ five_hour: { utilization: 2 }, seven_day: { utilization: 5 } }) });
  assert.equal(metric(second.probe(), "Session").used, 2);
  assert.equal(second.requests.length, 1);
});

test("Claude rejects malformed successful quota responses", () => {
  const app = load("claude", { credentials: claudeCredentials, request: () => response({}) });
  assert.throws(app.probe, /response invalid/);
  assert.equal(app.files.size, 0);
});

test("Claude local spend works without OAuth but never becomes a quota meter", () => {
  const app = load("claude", { ccusage: () => ({ status: "ok", data: { daily: [{ date: "2026-09-04", totalCost: 1.25, inputTokens: 100, outputTokens: 50, totalTokens: 150 }] } }) });
  const result = app.probe();
  assert.equal(app.requests.length, 0);
  assert.equal(result.source, "local");
  assert.ok(!lines(result).some((line) => line.type === "progress"));
  assert.equal(app.ingested.length, 1);
  assert.throws(load("claude").probe, /no local Claude usage/);
});

function cursorRequest(grok, status = 200) {
  return (req) => {
    if (req.url.endsWith("GetCurrentPeriodUsage")) return response({ enabled: true,
      planUsage: { totalPercentUsed: 10, autoPercentUsed: 5, apiPercentUsed: 15 } });
    if (req.url.endsWith("GetPlanInfo")) return response({ planInfo: { planName: "pro" } });
    if (req.url.endsWith("GetSandUsageStatus")) return response(grok, status);
    return response({});
  };
}

for (const id of ["cursor", "cursor-nightly"]) {
  test(`${id} retains its registration and adds Grok Bot usage after Other Models`, () => {
    const app = load(id, { credentials: { accessToken: "token" }, request: cursorRequest({ usagePercent: 0.5,
      nextResetTimestampUtc: "2026-09-10T00:00:00Z", currentPeriodStart: "2026-09-03T00:00:00Z" }) });
    const result = app.probe();
    assert.equal(app.plugin.id, id);
    assert.equal(metric(result, "Grok Bot usage").used, 0.5);
    const labels = lines(result).map((line) => line.label);
    assert.equal(labels.indexOf("Grok Bot usage"), labels.indexOf("Other Models") + 1);
  });
}

test("Cursor retains ordinary quotas when Grok Bot is unavailable or pooled", () => {
  for (const [body, status] of [[{}, 503], [{ usagePercent: 10, usesPooledEnterpriseAllowance: true }, 200], [{ usagePercent: null }, 200]]) {
    const result = load("cursor", { credentials: { accessToken: "token" }, request: cursorRequest(body, status) }).probe();
    assert.equal(metric(result, "Total usage").used, 10);
    assert.equal(metric(result, "Grok Bot usage"), undefined);
  }
});

function kiro(usage, overages) {
  return load("kiro", { files: { "~/.aws/sso/cache/kiro-auth-token.json": JSON.stringify({ accessToken: "token", profileArn: "arn:aws:codewhisperer:us-east-1:account:profile/test" }) },
    request: () => response({ nextDateReset: 1790812800, subscriptionInfo: { subscriptionTitle: "KIRO PRO" },
      overageConfiguration: overages, usageBreakdownList: [{ resourceType: "CREDIT", ...usage }] }) });
}

test("Kiro subtracts overages from included credits and preserves the billing reset", () => {
  const app = kiro({ currentUsageWithPrecision: 125, usageLimitWithPrecision: 100, currentOveragesWithPrecision: 25,
    overageCapWithPrecision: 50, overageCharges: 2.5, overageRate: 0.1, currency: "USD" }, { overageStatus: "ENABLED" });
  const result = app.probe();
  assert.equal(metric(result, "Credits").used, 100);
  assert.equal(metric(result, "Credits").resetsAt, "2026-10-01T00:00:00.000Z");
  assert.equal(metric(result, "Overage Credits").used, 25);
  assert.equal(metric(result, "Overage Charges").limit, 5);
  assert.equal(result.source, "api");
});

test("Kiro does not invent overage status or a zero-size allowance", () => {
  const result = kiro({ currentUsage: 5, usageLimit: 0, currentOverages: 5 }).probe();
  assert.equal(metric(result, "Overages"), undefined);
  assert.equal(metric(result, "Credits").type, "text");
  assert.equal(metric(result, "Overage Credits").value, "5 credits");
});

test("Qwen Cloud parses wrapped current ratios and subscription credit limits", () => {
  const app = load("qwencloud", { provider: { cookieHeader: "sec_token=csrf; session=token" }, request: (req) => {
    if (req.method === "GET") return response("<html>Account</html>");
    const api = new URL(req.url).searchParams.get("api");
    if (api.endsWith("/usage")) return response({ data: JSON.stringify({ per5HourPercentage: 0.005, per1WeekPercentage: 0.2, per5HourResetTime: "2026-09-05T15:00:00Z" }) });
    if (api.endsWith("/subscription")) return response({ specCode: "pro" });
    return response({ pro: { five_hour: 1000, weekly: 10000 } });
  } });
  const result = app.probe();
  assert.equal(metric(result, "5-hour").used, 0.5);
  assert.equal(metric(result, "Weekly").used, 20);
  assert.equal(result.plan, "Pro");
  assert.match(metric(result, "5-hour").detail, /1,000 credits/);
});

test("CodeBuddy never falls back to another account's cached credits", () => {
  const first = load("codebuddy", { provider: { cookieHeader: "session=one" }, request: () => response({ code: 0,
    data: { Response: { Data: { Accounts: [{ CapacitySize: 100, CapacityUsed: 10 }] } } } }) });
  first.probe();
  const second = load("codebuddy", { provider: { cookieHeader: "session=two" }, files: Object.fromEntries(first.files), request: () => response({}, 503) });
  assert.throws(second.probe, /temporary HTTP/);
});

test("OpenRouter persists token counts without double-counting reasoning and labels BYOK estimates", () => {
  const app = load("openrouter", { provider: { apiKey: "key", settings: { managementApiKey: "management" } }, request: (req) => {
    if (req.url.endsWith("/credits")) return response({ data: { total_credits: 10, total_usage: 1 } });
    if (req.url.endsWith("/key")) return response({});
    return response({ data: [{ date: "2026-09-04", model: "m", usage: 1, byok_usage_inference: 2,
      prompt_tokens: 100, completion_tokens: 20, reasoning_tokens: 5 }] });
  } });
  const result = app.probe();
  assert.equal(app.ingested[0].daily[0].totalTokens, 120);
  assert.equal(app.ingested[0].daily[0].outputTokens, 15);
  assert.equal(app.ingested[0].daily[0].reasoningOutputTokens, 5);
  assert.match(metric(result, "Cost").subtitle, /estimated BYOK/);
});

for (const [id, body, provider] of [
  ["clinepass", { success: true, data: { limits: [{ type: "five_hour", percentUsed: null }] } }, { apiKey: "key" }],
  ["zenmux", { success: true, data: { quota_5_hour: { usage_percentage: null } } }, { apiKey: "key" }],
  ["deepinfra", {}, { apiKey: "key" }],
  ["codebuddy", { code: 0, data: { Response: { Data: { Accounts: [{}] } } } }, { cookieHeader: "session=token" }],
]) {
  test(`${id} rejects absent numbers instead of showing zero usage`, () => {
    assert.throws(load(id, { provider, request: () => response(body) }).probe);
  });
}
