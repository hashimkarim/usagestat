const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { ROOT, load, metric, response } = require("./provider-sync-harness.cjs");

const newProviders = ["aiand", "deepinfra", "clinepass", "xai", "zenmux", "sub2api", "qoder", "sakana", "longcat", "notion", "qwencloud", "codebuddy", "zoommate", "ibmbob", "clawrouter", "wayfinder"];

test("all shipped manifests and provider scripts parse", () => {
  for (const dir of fs.readdirSync(path.join(ROOT, "plugins"), { withFileTypes: true }).filter((d) => d.isDirectory())) {
    const root = path.join(ROOT, "plugins", dir.name);
    const manifestPath = path.join(root, "plugin.json");
    if (!fs.existsSync(manifestPath)) continue;
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    assert.equal(manifest.id, dir.name);
    const entry = path.resolve(root, manifest.entry);
    new vm.Script(fs.readFileSync(entry, "utf8"), { filename: entry });
  }
});

test("new providers are opt-in, discoverable, and declare only allowed credentials", () => {
  const host = fs.readFileSync(path.join(ROOT, "crates/ai-usage-plugins/src/host_api.rs"), "utf8").split("const ENV_ALLOWLIST")[1].split("];", 1)[0];
  for (const id of newProviders) {
    const manifest = JSON.parse(fs.readFileSync(path.join(ROOT, "plugins", id, "plugin.json"), "utf8"));
    assert.equal(manifest.enabledByDefault, false, id);
    assert.ok(manifest.supportedModes.includes(manifest.autoMode), id);
    assert.equal(load(id).plugin.id, id);
    for (const name of manifest.envVars) assert.ok(host.includes(`"${name}"`), `${id}: ${name}`);
  }
});

test("sub2api saves only the dated daily total, never its lifetime total", () => {
  const app = load("sub2api", { provider: { apiKey: "key", workspaceId: "https://gateway.example" }, request: () => response({
    balance: 1, usage: { today: { requests: 1, total_tokens: 10, actual_cost: 2.5 }, total: { requests: 100, total_tokens: 1000, actual_cost: 100 } },
    expires_at: "2026-10-01T00:00:00Z",
  }) });
  const result = app.probe();
  assert.equal(app.ingested[0].daily.length, 1);
  assert.equal(app.ingested[0].daily[0].costUsd, 2.5);
  assert.equal(app.ingested[0].daily[0].totalTokens, 10);
  assert.equal(metric(result, "Expires").value, "2026-10-01T00:00:00.000Z");
});

test("sub2api rejects missing quota counters", () => {
  assert.throws(load("sub2api", { provider: { apiKey: "key", workspaceId: "https://gateway.example" }, request: () => response({ quota: { limit: 100 } }) }).probe, /invalid/);
});
