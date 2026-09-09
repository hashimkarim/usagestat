const { test } = require("node:test");
const assert = require("node:assert/strict");
const { load, metric, response, NOW } = require("./provider-sync-harness.cjs");

for (const [body, status, expected] of [
  [{ detail: { code: " INVALID_API_KEY ", status: "missing_permissions" } }, 401, /rejected/],
  [{ detail: { code: "unknown", status: " INSUFFICIENT_PERMISSIONS " } }, 401, /user_read/],
  [{ detail: { code: "missing_permissions" } }, 403, /user_read/],
  ["malformed response", 403, /IP allowlist/],
  [{ detail: { message: "private-response-sentinel" } }, 401, /could not authenticate/],
]) {
  test(`ElevenLabs explains ${status} ${JSON.stringify(body)} without exposing the response`, () => {
    const app = load("elevenlabs", { provider: { apiKey: "configured-key" }, request: () => response(body, status) });
    assert.throws(app.probe, (error) => expected.test(String(error)) && !String(error).includes("private-response-sentinel"));
    assert.equal(app.requests.length, 1);
    assert.equal(app.requests[0].headers["xi-api-key"], "configured-key");
  });
}

for (const [region, amount, expected] of [["cn", -2.5, "CNY -2.50"], ["global", 0, "USD 0.00"]]) {
  test(`Moonshot labels ${region} balances in their original currency`, () => {
    const app = load("moonshot", { provider: { apiKey: "key", region }, request: () => response({ data: { available_balance: amount } }) });
    assert.equal(metric(app.probe(), "Balance").value, expected);
  });
}

test("Moonshot follows the actual endpoint currency when a URL overrides the region", () => {
  const app = load("moonshot", { provider: { apiKey: "key", region: "cn" }, env: { MOONSHOT_API_URL: "https://api.moonshot.ai" },
    request: () => response({ available_balance: 1.5 }) });
  assert.equal(metric(app.probe(), "Balance").value, "USD 1.50");
});

function kiro(profileArn) {
  return load("kiro", { files: { "~/.aws/sso/cache/kiro-auth-token.json": JSON.stringify({ accessToken: "key", profileArn }) },
    request: () => response({ usageBreakdownList: [{ resourceType: "CREDIT", currentUsage: 10, usageLimit: 100 }] }) });
}
for (const region of ["us-east-1", "eu-central-1"]) {
  test(`Kiro keeps IDE usage requests in the selected ${region} profile region`, () => {
    const arn = `arn:aws:codewhisperer:${region}:123456789012:profile/test`;
    const app = kiro(arn);
    assert.equal(metric(app.probe(), "Credits").used, 10);
    const url = new URL(app.requests[0].url);
    assert.equal(url.hostname, `q.${region}.amazonaws.com`);
    assert.equal(url.searchParams.get("profileArn"), arn);
  });
}
for (const arn of [
  "arn:aws:codewhisperer:us-west-2:123456789012:profile/test",
  "arn:aws:s3:us-east-1:123456789012:profile/test",
  "arn:aws:codewhisperer:us-east-1:123456789012:profile/",
  "arn:aws:codewhisperer:evil.example/:123456789012:profile/test",
  "arn:aws:codewhisperer:us-east-1:123456789012:profile/te\nst",
]) {
  test(`Kiro rejects invalid profile routing before sending credentials: ${JSON.stringify(arn)}`, () => {
    const app = kiro(arn);
    assert.throws(app.probe, /profile ARN/);
    assert.equal(app.requests.length, 0);
  });
}

const remains = { model_remains: [{ current_interval_total_count: 100, current_interval_usage_count: 75 }] };
test("MiniMax prefers the current coding-plan API with an explicitly configured key", () => {
  const app = load("minimax", { provider: { apiKey: "configured" }, env: { MINIMAX_API_KEY: "other" }, request: () => response(remains) });
  const result = app.probe();
  assert.equal(result.source, "api");
  assert.equal(metric(result, "Session").used, 25);
  assert.equal(app.requests.length, 1);
  assert.equal(app.requests[0].url, "https://platform.minimax.io/v1/api/openplatform/coding_plan/remains");
  assert.equal(app.requests[0].headers.Authorization, "Bearer configured");
});

for (const failure of [401, 403, 429, 503, "network"]) {
  test(`MiniMax ${failure} failures do not retry other hosts or regions`, () => {
    const app = load("minimax", { provider: { apiKey: "key", region: "cn" }, request: () => {
      if (failure === "network") throw new Error("offline");
      return response({}, failure);
    } });
    assert.throws(app.probe);
    assert.equal(app.requests.length, 1);
    assert.equal(new URL(app.requests[0].url).hostname, "platform.minimaxi.com");
  });
}

test("MiniMax can fall back within a region when the preferred endpoint is incompatible", () => {
  const app = load("minimax", { provider: { apiKey: "key" }, request: (_, n) => n === 1 ? response({}, 404) : response(remains) });
  assert.equal(metric(app.probe(), "Session").used, 25);
  assert.equal(app.requests.length, 2);
  assert.equal(new URL(app.requests[1].url).hostname, "www.minimax.io");
});

test("Poe skips invalid dates and supports seconds, milliseconds, microseconds and ISO timestamps", () => {
  const ms = Date.parse(NOW);
  const times = [ms / 1000, ms, ms * 1000, String(ms / 1000), NOW];
  const app = load("poe", { provider: { apiKey: "key" }, request: (req) => req.url.includes("current_balance")
    ? response({ current_point_balance: 2500 })
    : response({ data: [null, { creation_time: 1e300, cost_points: 999 }, { creation_time: "-1e300", cost_points: 999 },
      ...times.map((creation_time, i) => ({ creation_time, cost_points: 2 ** i }))] }) });
  const result = app.probe();
  assert.equal(metric(result, "Balance").value, "2,500 points");
  assert.equal(metric(result, "Today").value, "31 points · 5 requests");
  assert.equal(metric(result, "Last 7 Days").value, "31 points · 5 requests");
  assert.equal(metric(result, "Point History").points[0].value, 31);
});

test("Poe weekly totals cover elapsed days, including sparse and empty weeks", () => {
  const ms = Date.parse(NOW);
  for (const recent of [true, false]) {
    const rows = [{ creation_time: ms - 10 * 86400000, cost_points: 100 }, { creation_time: ms - 8 * 86400000, cost_points: 200 }];
    if (recent) rows.push({ creation_time: ms - 86400000, cost_points: 3, cost_usd: 0.25 });
    const app = load("poe", { provider: { apiKey: "key" }, request: (req) => req.url.includes("current_balance")
      ? response({ current_point_balance: 10 }) : response({ data: rows }) });
    const result = app.probe();
    assert.equal(metric(result, "Last 7 Days").value, recent ? "3 points · $0.25 · 1 requests" : "0 points · 0 requests");
    assert.equal(metric(result, "Today").value, "0 points · 0 requests");
  }
});

test("OpenRouter rejects row, daily and cross-day token overflow while keeping valid quota", () => {
  const row = { date: "2026-09-04", model: "m", usage: 1, prompt_tokens: Number.MAX_SAFE_INTEGER, completion_tokens: 1 };
  for (const rows of [[row], [{ ...row, completion_tokens: 0 }, { ...row, model: "other", prompt_tokens: 1, completion_tokens: 0 }],
    [{ ...row, completion_tokens: 0 }, { ...row, date: "2026-09-03", prompt_tokens: 1, completion_tokens: 0 }]]) {
    const app = load("openrouter", { provider: { apiKey: "key", settings: { managementApiKey: "management" } }, request: (req) => {
      if (req.url.endsWith("/credits")) return response({ data: { total_credits: 100, total_usage: 10 } });
      if (req.url.endsWith("/key")) return response({ data: { limit: 20, limit_remaining: 17 } });
      return response({ data: rows });
    } });
    const result = app.probe();
    assert.equal(metric(result, "Credits").used, 10);
    assert.equal(metric(result, "Key Cap").used, 3);
    assert.equal(metric(result, "Cost"), undefined);
    assert.equal(app.ingested.length, 0);
  }
});

test("Command Code monthly usage survives optional subscription failure", () => {
  const app = load("command-code", { provider: { apiKey: "key" }, request: (req) => {
    if (req.url.endsWith("/subscriptions")) return response({}, 503);
    if (req.url.endsWith("/credits")) return response({ credits: { monthlyCredits: 60 } });
    if (req.url.endsWith("/summary")) return response({ totalMonthlyCredits: 20 });
    return response({ id: "account" });
  } });
  assert.equal(metric(app.probe(), "Monthly credits").used, 25);
});

test("Command Code never treats an unknown monthly used amount as zero", () => {
  const app = load("command-code", { provider: { apiKey: "key" }, request: (req) => {
    if (req.url.endsWith("/subscriptions")) return response({ success: true, data: { planId: "individual-pro-v1" } });
    if (req.url.endsWith("/credits")) return response({ credits: { monthlyCredits: 60 } });
    if (req.url.endsWith("/summary")) return response({ totalMonthlyCredits: null });
    return response({ id: "account" });
  } });
  const result = app.probe();
  assert.equal(metric(result, "Monthly credits"), undefined);
  assert.equal(result.plan, "Pro");
});
