const { test } = require("node:test");
const assert = require("node:assert/strict");
const { load, lines, metric, response, NOW } = require("./provider-sync-harness.cjs");

const usage = {
  usage: { used: "20", limit: "100", resetTime: "2026-09-10T00:00:00Z" },
  limits: [{ window: { duration: 300, timeUnit: "TIME_UNIT_MINUTE" }, detail: { remaining: 99, limit: 100 } }],
  user: { membership: { level: "LEVEL_BASIC" } },
};
const credentialPath = "~/.kimi-code/credentials/kimi-code.json";
const credential = JSON.stringify({ access_token: "cli-token", refresh_token: "never-use", expires_at: Date.parse(NOW) / 1000 + 3600 });
const files = { [credentialPath]: credential, "~/.kimi-code/device_id": "device-1" };

test("Kimi Code API keys take precedence over CLI credentials", () => {
  const app = load("kimi", { env: { KIMI_CODE_API_KEY: "code-key" }, files, request: () => response(usage) });
  const result = app.probe();
  assert.equal(result.source, "api");
  assert.equal(result.plan, "Moderato");
  assert.equal(metric(result, "Weekly").periodDurationMs, 7 * 86400000);
  assert.equal(metric(result, "Session").used, 1);
  assert.equal(app.requests[0].headers.Authorization, "Bearer code-key");
  assert.equal(app.requests[0].headers["X-Msh-Device-Id"], undefined);
  assert.deepEqual(Object.fromEntries(app.files), files);
});

test("Kimi CLI authentication is read-only and preserves the device identity", () => {
  const app = load("kimi", { files, request: () => response(usage) });
  assert.equal(app.probe().source, "oauth");
  assert.equal(app.requests[0].headers.Authorization, "Bearer cli-token");
  assert.equal(app.requests[0].headers["X-Msh-Device-Id"], "device-1");
  assert.equal(app.requests[0].headers["X-Msh-Platform"], "kimi_code_cli");
  assert.equal(app.requests.length, 1);
  assert.deepEqual(Object.fromEntries(app.files), files);
});

test("Kimi never rotates expired or rejected CLI credentials", () => {
  const expiredFiles = { ...files, [credentialPath]: JSON.stringify({ access_token: "expired", refresh_token: "never-use", expires_at: Date.parse(NOW) / 1000 }) };
  const expired = load("kimi", { files: expiredFiles, request: () => { throw new Error("must not refresh"); } });
  assert.throws(expired.probe, /fresh Kimi/);
  assert.equal(expired.requests.length, 0);
  assert.deepEqual(Object.fromEntries(expired.files), expiredFiles);
  const rejected = load("kimi", { files, request: () => response({}, 401) });
  assert.throws(rejected.probe, /expired/);
  assert.equal(rejected.requests.length, 1);
  assert.deepEqual(Object.fromEntries(rejected.files), files);
});

test("Kimi endpoint overrides cannot receive CLI tokens", () => {
  for (const name of ["KIMI_CODE_BASE_URL", "KIMI_CODE_OAUTH_HOST", "KIMI_OAUTH_HOST"]) {
    const app = load("kimi", { files, env: { [name]: "https://custom.example" } });
    assert.throws(app.probe, /explicit API key/);
    assert.equal(app.requests.length, 0);
    assert.deepEqual(Object.fromEntries(app.files), files);
  }
  const app = load("kimi", { provider: { apiKey: "explicit", settings: { baseUrl: "https://custom.example/coding/v1" } }, request: () => response(usage) });
  app.probe();
  assert.equal(app.requests[0].url, "https://custom.example/coding/v1/usages");
});

test("Kimi web membership enriches quotas without duplicating matching weekly limits", () => {
  const app = load("kimi", { source: "web", files, provider: { cookieHeader: "kimi-auth=web-token" }, request: (req) => req.url.endsWith("GetUsages")
    ? response({ usages: [{ scope: "FEATURE_CODING", detail: usage.usage, limits: usage.limits }] })
    : response({ subscriptionBalance: { amountUsedRatio: 0.125, kimiCodeUsedRatio: 0.01, feature: "FEATURE_OMNI", expireTime: "2026-10-01T00:00:00Z" },
      ratelimitCode7d: { enabled: true, ratio: 0.2, resetTime: usage.usage.resetTime } }) });
  const result = app.probe();
  assert.equal(result.source, "web");
  assert.equal(metric(result, "Monthly").used, 12.5);
  assert.equal(metric(result, "Code 7-day"), undefined);
  assert.equal(app.requests[0].headers.Authorization, "Bearer web-token");
  assert.equal(app.requests[1].timeoutMs, 2000);
  assert.deepEqual(Object.fromEntries(app.files), files);
});

test("Kimi optional membership failures preserve Code API quotas and cookie-source off skips enrichment", () => {
  const request = (req) => req.method === "GET" ? response(usage) : (() => { throw new Error("offline"); })();
  const app = load("kimi", { provider: { apiKey: "key", cookieHeader: "kimi-auth=web-token" }, request });
  assert.equal(metric(app.probe(), "Weekly").used, 20);
  const off = load("kimi", { provider: { apiKey: "key", cookieHeader: "kimi-auth=web-token", settings: { cookieSource: "off" } }, request });
  assert.equal(lines(off.probe()).length, 2);
  assert.equal(off.requests.length, 1);
});

test("Kimi never substitutes Open Platform keys or missing counters for Code quotas", () => {
  const platform = load("kimi", { env: { KIMI_API_KEY: "platform-key" } });
  assert.throws(platform.probe, /KIMI_CODE_API_KEY/);
  assert.equal(platform.requests.length, 0);
  for (const quota of [{ limit: 100, used: null }, { limit: 100, used: false }, { limit: 100, remaining: 101 }]) {
    assert.throws(load("kimi", { provider: { apiKey: "key" }, request: () => response({ usage: quota }) }).probe, /measurable quota/);
  }
});
