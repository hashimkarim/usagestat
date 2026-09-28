const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { createHash } = require("node:crypto");

const ROOT = path.resolve(__dirname, "..");
const hostSource = fs.readFileSync(path.join(ROOT, "crates/ai-usage-plugins/src/host_api.rs"), "utf8");
// Run the shipped JS helpers so fixture tests catch host/plugin contract mismatches.
const utilityScript = hostSource.match(/fn inject_utils[\s\S]*?r#"([\s\S]*?)"#/)[1];
const NOW = "2026-09-05T12:00:00Z";

function response(body, status = 200) {
  return { status, bodyText: typeof body === "string" ? body : JSON.stringify(body), headers: {} };
}

function load(id, options = {}) {
  const requests = [];
  const ingested = [];
  const logs = [];
  const files = new Map(Object.entries(options.files || {}));
  const now = options.now || NOW;
  class Clock extends Date {
    constructor(...args) { super(...(args.length ? args : [now])); }
    static now() { return Date.parse(now); }
  }
  const ctx = {
    nowIso: now, sourceMode: options.source || "auto",
    app: { version: "test", platform: "linux", appDataDir: "/test/data", pluginDataDir: "/test/data/" + id },
    provider: options.provider || {},
    host: {
      env: { get: (name) => options.env && options.env[name] || null },
      log: Object.fromEntries(["info", "warn", "error"].map((name) => [name, (text) => logs.push(text)])),
      http: {
        validateProviderUrl: (raw, serialized) => {
          const url = new URL(raw);
          if (/[\\\s]/.test(raw) || url.username || url.password || url.hash) throw new Error('Invalid provider URL');
          const allowed = JSON.parse(serialized).some(endpoint => {
            if (!endpoint.url) return false;
            const base = new URL(endpoint.url), prefix = base.pathname.replace(/\/+$/, '');
            return base.origin === url.origin && (!prefix || url.pathname === prefix || url.pathname.startsWith(prefix + '/'));
          });
          if (!allowed) throw new Error('Provider request is outside its declared endpoints');
          return url.href;
        },
        request: (req) => {
          requests.push(req);
          if (!options.request) return response({}, 404);
          return options.request(req, requests.length);
        },
        validateBaseUrl: (raw, allowHttp) => {
          const url = new URL(raw);
          const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
          if (/[\\\s]/.test(raw) || url.username || url.password || url.search || url.hash ||
              !(url.protocol === "https:" || allowHttp && url.protocol === "http:" && loopback)) throw new Error("Invalid base URL");
          return url.href.replace(/\/+$/, "");
        },
      },
      credentials: { get: () => options.credentials ? JSON.stringify(options.credentials) : null, update: (raw) => { options.credentials = JSON.parse(raw); } },
      fs: {
        homeDir: "/test/home",
        appSupportPath: (relative) => "/test/support/" + relative,
        exists: (name) => files.has(name),
        readText: (name) => {
          if (!files.has(name)) throw new Error("ENOENT");
          return files.get(name);
        },
        writeText: (name, value) => files.set(name, value),
        readTextLimited: (name, limit) => { const text = files.get(name); if (text == null || text.length > limit) throw new Error('Unreadable'); return text; },
        listDir: (name) => [...new Set([...files.keys()].filter((file) => file.startsWith(name + "/")).map((file) => file.slice(name.length + 1).split('/')[0]))],
        firstExisting: (names) => names.find((name) => files.has(name)) || null,
        firstExistingAppSupport: () => null,
      },
      keychain: { readGenericPassword: () => null, readGenericPasswordForCurrentUser: () => null, listGenericPasswords: () => [] },
      sqlite: { query: options.sqlite || (() => "[]") },
      command: { run: options.command || (() => ({ status: 1, stdout: "", stderr: "" })) },
      claude: options.claude,
      ccusage: { query: options.ccusage || (() => ({ status: "no_runner" })) },
      usageDaily: { ingest: (payload) => ingested.push(payload) },
      crypto: { sha256: (value) => createHash("sha256").update(value).digest("hex"), sha1Hex: (value) => createHash("sha1").update(value).digest("hex") },
    },
  };
  const sandbox = vm.createContext({ __usagestat_ctx: ctx, __OPENUSAGE_PLUGIN_REGISTRATION_ID__: id, Date: Clock });
  vm.runInContext(utilityScript, sandbox);
  vm.runInContext(fs.readFileSync(path.join(ROOT, 'crates/ai-usage-plugins/src/bundled_provider.js'), 'utf8'), sandbox);
  vm.runInContext(fs.readFileSync(path.join(ROOT, "plugins", id, "plugin.js"), "utf8"), sandbox, { filename: id + "/plugin.js" });
  const plugin = sandbox.__usagestat_plugin || sandbox.__ai_usage_plugin || sandbox.__openusage_plugin;
  return { ctx, requests, ingested, logs, files, plugin,
    probe: () => {
      const result = plugin.probe(ctx);
      return result && typeof result.then === 'function' ? result.then(value => JSON.parse(JSON.stringify(value))) : JSON.parse(JSON.stringify(result));
    } };
}

function lines(output) { return output.lines || output.metrics; }
function metric(output, label) { return lines(output).find((line) => line.label === label); }

module.exports = { load, lines, metric, response, ROOT, NOW };
