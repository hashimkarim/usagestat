const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const crypto = require('node:crypto');

const root = path.resolve(__dirname, '..');
// Run the same JS utility functions injected by the native host, rather than
// another implementation of date, refresh, output and request semantics.
const nativeHost = fs.readFileSync(path.join(root, 'crates/ai-usage-plugins/src/host_api.rs'), 'utf8');
const utilityScript = nativeHost.slice(nativeHost.indexOf('fn inject_utils(')).match(/r#"([\s\S]*?)"#/)[1];

function providerHarness(id, options = {}) {
  const platform = options.platform || 'linux';
  if (!['linux', 'macos', 'windows'].includes(platform)) throw new Error('Unknown fixture OS');
  const paths = platform === 'windows' ? path.win32 : path.posix;
  const home = options.home || (platform === 'windows' ? 'C:\\Users\\Fixture 使用' : '/home/Fixture 使用');
  const appSupport = options.appSupport || (platform === 'windows'
    ? 'D:\\Redirected AppData 使用\\Roaming'
    : platform === 'macos' ? home + '/Library/Application Support' : '/redirected XDG 使用');
  const env = {...options.env};
  const calls = {files: [], sqlite: [], http: [], logs: [], keychain: [], writes: []};
  const normalize = value => paths.normalize(value === '~' ? home : value.startsWith('~/') || value.startsWith('~\\') ? paths.join(home, value.slice(2)) : value);
  const files = new Map(Object.entries(options.files || {}).map(([name, value]) => [normalize(name), value]));
  const databases = new Map(Object.entries(options.databases || {}).map(([name, value]) => [normalize(name), value]));
  function exists(name) {
    const normalized = normalize(name);
    calls.files.push(normalized);
    return files.has(normalized) || databases.has(normalized) || [...files.keys(), ...databases.keys()].some(key => key.startsWith(normalized + paths.sep));
  }
  const host = {
    crypto: {sha256Hex: value => crypto.createHash('sha256').update(value, 'utf8').digest('hex')},
    env: {get: name => env[name] || null},
    fs: {
      homeDir: home,
      localAppDataPath: relative => paths.join(options.localAppData || (platform === 'windows' ? 'E:\\Redirected Local 使用' : home + '/.local/share'), relative),
      appSupportPath: relative => options.appSupportUnavailable ? null : paths.join(appSupport, relative),
      exists,
      readText(name) { const key = normalize(name); calls.files.push(key); if (!files.has(key)) throw new Error('fixture file missing'); return files.get(key); },
      writeText(name, value) { const key = normalize(name); calls.writes.push(key); files.set(key, value); },
      listDir(name) {
        const key = normalize(name); calls.files.push(key);
        return [...new Set([...files.keys(), ...databases.keys()].filter(file => file.startsWith(key + paths.sep)).map(file => file.slice(key.length + 1).split(paths.sep)[0]))];
      },
      firstExisting(names) { return names.find(exists) || null; },
      firstExistingAppSupport(relative) { const name = paths.join(appSupport, relative); return exists(name) ? name : null; },
    },
    sqlite: {query(name, sql) { const key = normalize(name); calls.sqlite.push({path: key, sql}); if (!databases.has(key)) throw new Error('fixture database missing'); const rows=databases.get(key); return JSON.stringify(sql.includes('sqlite_master')&&Array.isArray(rows)?[{name:'message'}]:rows); }},
    http: {request(request) { calls.http.push(request); if (!options.http) throw new Error('Unexpected external request in fixture'); return options.http(request); }},
    log: Object.fromEntries(['info', 'warn', 'error', 'debug'].map(level => [level, value => calls.logs.push({level, value})])),
    keychain: options.keychain || {},
    ...options.host,
  };
  const ctx = {app: {platform, version: '1.0.3'}, nowIso: '2026-08-01T12:00:00Z', provider: {id, settings: options.settings || {}}, host};
  const sandbox = vm.createContext({__usagestat_ctx: ctx, TextDecoder, TextEncoder, Uint8Array});
  vm.runInContext(utilityScript, sandbox, {timeout: 1000});
  const manifest = JSON.parse(fs.readFileSync(path.join(root, 'plugins', id, 'plugin.json'), 'utf8'));
  vm.runInContext(fs.readFileSync(path.resolve(root, 'plugins', id, manifest.entry), 'utf8'), sandbox, {timeout: 1000});
  return {ctx, calls, home, appSupport, normalize, files, databases, probe() {
    return vm.runInContext('(globalThis.__usagestat_plugin || globalThis.__openusage_plugin).probe(__usagestat_ctx)', sandbox, {timeout: 1000});
  }};
}

module.exports = {providerHarness};
