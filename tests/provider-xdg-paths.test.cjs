const {test} = require('node:test');
const assert = require('node:assert/strict');
const {providerHarness} = require('./provider-harness.cjs');
const json = value => ({status: 200, bodyText: JSON.stringify(value)});

for (const platform of ['linux', 'macos', 'windows']) {
  test(`${platform}: OpenCode uses upstream XDG roots and one selected persisted history`, () => {
    const root = platform === 'windows' ? 'D:\\Redirected data 使用' : '/redirected data 使用';
    const h = providerHarness('opencode-go', {platform, env: {XDG_DATA_HOME: root}});
    const db = h.normalize(root + '/opencode/opencode.db');
    h.databases.set(db, [{createdMs: Date.now() - 60000, cost: 3}]);
    assert.equal(h.probe().lines[0].used, 25);
    assert(h.calls.sqlite.every(call => call.path === db));
    assert(h.calls.files.every(path => !path.includes('.local')));

    h.ctx.provider.settings = {dataDir: h.home + '/selected profile'};
    const selected = h.normalize(h.home + '/selected profile/opencode.db');
    h.databases.set(selected, [{createdMs: Date.now() - 60000, cost: 6}]);
    h.calls.sqlite.length = 0;
    assert.equal(h.probe().lines[0].used, 50);
    assert(h.calls.sqlite.every(call => call.path === selected));

    h.ctx.provider.settings = {dataDir: h.home + '/missing'};
    const before = h.calls.http.length;
    assert.throws(() => h.probe(), e => /not detected/.test(String(e)));
    assert.equal(h.calls.http.length, before);
    h.ctx.provider.settings = {dataDir: 'relative-profile'};
    assert.throws(() => h.probe(), e => e.code === 'failed');
    assert.equal(h.calls.writes.length, 0);
  });

  test(`${platform}: OpenCode database and inline auth overrides cannot select a fallback account`, () => {
    const env = {OPENCODE_DB: 'channel.db', OPENCODE_AUTH_CONTENT: '{"opencode-go":{"type":"api","key":"synthetic"}}'};
    const h = providerHarness('opencode-go', {platform, env});
    h.ctx.sourceMode = 'local';
    const db = h.normalize(h.home + '/.local/share/opencode/channel.db');
    h.databases.set(db, [{createdMs: Date.now() - 60000, cost: 1.2}]);
    assert.equal(h.probe().lines[0].used, 10);
    assert(h.calls.sqlite.every(call => call.path === db));
    assert.equal(h.calls.files.length, 0);

    h.ctx.host.env.get = name => name === 'OPENCODE_AUTH_CONTENT' ? '{broken' : null;
    assert.throws(() => h.probe(), e => e.code === 'credential-malformed');
    assert.equal(h.calls.files.length, 0);
    h.ctx.host.env.get = name => name === 'OPENCODE_DB' ? ':memory:' : null;
    assert.throws(() => h.probe(), e => e.code === 'unsupported');
    h.ctx.provider.settings = {databasePath: db};
    assert.equal(h.probe().lines[0].used, 10);
    h.ctx.provider.settings = {databasePath: 'relative.db'};
    assert.throws(() => h.probe(), e => e.code === 'failed');
    assert(!JSON.stringify(h.calls.logs).includes('synthetic'));
  });

  test(`${platform}: Zed uses native settings, JSONC and authoritative custom data directories`, () => {
    const h = providerHarness('zed', {platform, settings: {userId: 'fixture', accessToken: 'synthetic'},
      http: () => json({user: {name: 'Fixture'}, plan: {usage: {edit_predictions: {used: 25, limit: 100}}}})});
    const standard = platform === 'macos' ? h.home + '/.config/zed/settings.json'
      : h.ctx.host.fs.appSupportPath((platform === 'windows' ? 'Zed' : 'zed') + '/settings.json');
    h.files.set(h.normalize(standard), '\uFEFF{/* selected */ "server_url": "https://custom.example.test", // comment\n "text": "escaped \\" and // /* ,}", "array": [1,2,], }');
    assert.equal(h.probe().lines[0].used, 25);
    assert.equal(h.calls.http.at(-1).url, 'https://custom.example.test/client/users/me');

    h.ctx.provider.settings.userDataDir = h.home + '/selected Zed';
    const selected = h.normalize(h.home + '/selected Zed/config/settings.json');
    h.files.set(selected, '{"server_url":"https://selected.example.test"}');
    h.calls.files.length = 0;
    h.probe();
    assert.equal(h.calls.http.at(-1).url, 'https://selected.example.test/client/users/me');
    assert(h.calls.files.every(path => path === selected));
    const before = h.calls.http.length;
    h.files.set(selected, '{/* unclosed');
    assert.throws(() => h.probe(), e => e.code === 'credential-unavailable');
    h.files.delete(selected);
    assert.throws(() => h.probe(), e => e.code === 'credential-unavailable');
    h.ctx.provider.settings.userDataDir = 'relative';
    assert.throws(() => h.probe(), e => e.code === 'failed');
    assert.equal(h.calls.http.length, before);
    assert.equal(h.calls.writes.length, 0);
  });
}

test('Zed Linux Flatpak config root overrides host XDG; explicit settingsPath overrides both', () => {
  const h = providerHarness('zed', {env: {FLATPAK_XDG_CONFIG_HOME: '/flatpak 使用'},
    settings: {userId: 'fixture', accessToken: 'synthetic'}, http: () => json({})});
  h.files.set('/flatpak 使用/zed/settings.json', '{}');
  h.probe();
  assert(h.calls.files.every(path => path === '/flatpak 使用/zed/settings.json'));
  h.ctx.provider.settings.settingsPath = '/selected.json';
  h.files.set('/selected.json', '{}');
  h.calls.files.length = 0;
  h.probe();
  assert(h.calls.files.every(path => path === '/selected.json'));
});
