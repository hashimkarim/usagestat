const test = require('node:test');
const assert = require('node:assert/strict');
const {load, metric, response, NOW} = require('./provider-sync-harness.cjs');
const credentials = {claudeAiOauth: {accessToken: 'fake-profile-token', scopes: ['user:profile'], subscriptionType: 'pro'}};
const files = {'~/.claude/.credentials.json': JSON.stringify(credentials)};
const quota = (at = Date.parse(NOW) - 60_000, source = 'cli-cache') => JSON.stringify({
  data: {five_hour: {utilization: 19, resets_at: '2026-09-05T16:00:00Z'}, seven_day: {utilization: 2, resets_at: '2026-09-10T16:00:00Z'}}, fetchedAtMs: at, source,
});

test('Claude auto reads standalone quota without an API request or T3', () => {
  const app = load('claude', {files, claude: {readQuota: () => quota(), probeQuota: () => assert.fail('fresh cache needs no CLI process')}});
  const result = app.probe();
  assert.equal(result.state, 'ready');
  assert.equal(result.source, 'cli-cache');
  assert.equal(result.fetchedAt, new Date(Date.parse(NOW) - 60_000).toISOString());
  assert.equal(metric(result, 'Session').used, 19);
  assert.equal(metric(result, 'Weekly').used, 2);
  assert.equal(app.requests.length, 0);
});

test('Claude auto falls back to its own CLI check on an OAuth transport failure', () => {
  let calls = 0;
  const app = load('claude', {files, request: () => response({}, 503), claude: {readQuota: () => null, probeQuota: () => { calls++; return quota(Date.parse(NOW), 'cli'); }}});
  assert.equal(app.probe().source, 'cli');
  assert.equal(calls, 1);
  assert.equal(app.requests.length, 1);
});

test('Claude does not add a second CLI request when OAuth succeeds', () => {
  const app = load('claude', {files, request: () => response({five_hour: {utilization: 12}}),
    claude: {readQuota: () => null, probeQuota: () => assert.fail('working OAuth needs no CLI request')}});
  assert.equal(metric(app.probe(), 'Session').used, 12);
});

test('Claude cache works during Retry-After without clearing or bypassing it', () => {
  const limited = load('claude', {files, request: () => ({...response({}, 429), headers: {'retry-after': '3600'}})});
  assert.equal(limited.probe().state, 'failed');
  const cachedFiles = Object.fromEntries(limited.files);
  const cache = '/test/data/claude/live-usage-cache.json';
  const app = load('claude', {files: cachedFiles, claude: {readQuota: () => quota(), probeQuota: () => assert.fail('must respect cooldown')}});
  assert.equal(app.probe().source, 'cli-cache');
  assert.equal(app.requests.length, 0);
  assert.equal(app.files.get(cache), cachedFiles[cache]);
  const stale = load('claude', {files: cachedFiles, claude: {readQuota: () => quota(Date.parse(NOW) - 3600_000), probeQuota: () => assert.fail('must respect cooldown')}});
  assert.equal(stale.probe().state, 'failed');
  assert.equal(stale.requests.length, 0);
});

test('Claude never uses the ambient CLI for a named account', () => {
  const app = load('claude', {files, credentials: {accessToken: 'named-account'},
    claude: {readQuota: () => assert.fail('ambient account'), probeQuota: () => assert.fail('ambient account')},
    request: () => response({five_hour: {utilization: 34}})});
  assert.equal(metric(app.probe(), 'Session').used, 34);
});

test('Claude explicit OAuth and local modes never start a CLI quota probe', () => {
  for (const source of ['oauth', 'local']) {
    const app = load('claude', {source, files, claude: {readQuota: () => assert.fail('wrong mode'), probeQuota: () => assert.fail('wrong mode')},
      request: () => response({five_hour: {utilization: 4}})});
    app.probe();
  }
});

test('Claude CLI failures fall back to OAuth without fabricating zero quotas', () => {
  for (const cli of [null, '{invalid', JSON.stringify({data: {}, fetchedAtMs: Date.parse(NOW), source: 'cli'}), quota(Date.parse(NOW) + 1)]) {
    const app = load('claude', {files, claude: {readQuota: () => null, probeQuota: () => cli},
      request: () => response({five_hour: {utilization: 12}})});
    const result = app.probe();
    assert.equal(metric(result, 'Session').used, 12);
    assert.equal(result.source, 'oauth');
  }
});

test('Claude explicit CLI mode reports absent data instead of a ready local-only result', () => {
  const app = load('claude', {files, source: 'cli', claude: {readQuota: () => null, probeQuota: () => null}});
  assert.throws(() => app.probe(), /no fresh quota data/);
  assert.equal(app.requests.length, 0);
});
