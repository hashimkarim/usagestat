const { test } = require('node:test');
const assert = require('node:assert/strict');
const { providerHarness } = require('./provider-harness.cjs');

const quota = { daily_percentage: 12, weekly_percentage: 34, plan_name: 'Pro',
  daily_reset_at: '2026-09-12T00:00:00Z', weekly_reset_at: 1789776000, overage_balance_cents: 125 };
const response = (body = quota, status = 200) => ({status, bodyText: JSON.stringify(body)});

function manual(options = {}) {
  const env = options.env || {};
  const h = providerHarness('devin', {
    ...options,
    settings: {authSource: 'manual', ...options.settings},
    http: options.http || (() => response()),
    host: {env: {get: name => Object.hasOwn(env, name) ? env[name] : null}},
  });
  Object.assign(h.ctx.provider, {cookieHeader: 'Authorization: Bearer fixture-token', workspaceId: 'org_fixture'}, options.provider);
  h.ctx.sourceMode = options.mode || 'auto';
  return h;
}

for (const platform of ['linux', 'macos', 'windows']) {
  test(`${platform}: Devin configured web bearer and organization reach the quota request`, () => {
    const h = manual({platform});
    const result = h.probe();
    assert.equal(result.source, 'web');
    assert.equal(result.plan, 'Pro');
    assert.deepEqual(Array.from(result.lines, row => [row.label, row.used ?? row.value]), [
      ['Daily quota', 12], ['Weekly quota', 34], ['Extra usage balance', '$1.25'],
    ]);
    assert.equal(result.lines[0].resetsAt, '2026-09-12T00:00:00.000Z');
    assert.equal(result.lines[1].periodDurationMs, 7 * 86400000);
    assert.equal(h.calls.http.length, 1);
    assert.equal(h.calls.http[0].url, 'https://app.devin.ai/api/org_fixture/billing/quota/usage');
    assert.equal(h.calls.http[0].headers.Authorization, 'Bearer fixture-token');
    assert.equal(h.calls.http[0].headers['x-cog-org-id'], 'org_fixture');
    assert.equal(h.calls.files.length + h.calls.sqlite.length + h.calls.writes.length, 0);
  });
}

test('Devin environment overrides preserve both token and organization precedence', () => {
  const h = manual({env: {DEVIN_BEARER_TOKEN: 'env-token', DEVIN_AUTHORIZATION: 'ignored', DEVIN_ORGANIZATION: 'org/acme', DEVIN_ORG: 'ignored'}});
  h.probe();
  assert.equal(h.calls.http[0].url, 'https://app.devin.ai/api/org/acme/billing/quota/usage');
  assert.equal(h.calls.http[0].headers.Authorization, 'Bearer env-token');
  assert.equal(h.calls.http[0].headers['x-cog-org-id'], undefined);
  const aliases = manual({env: {DEVIN_AUTHORIZATION: 'Bearer alias-token', DEVIN_ORG: 'other'}});
  aliases.probe();
  assert.equal(aliases.calls.http[0].headers.Authorization, 'Bearer alias-token');
  assert.match(aliases.calls.http[0].url, /\/org\/other\//);
});

test('Devin empty overrides cannot fall back to another configured token or organization', () => {
  for (const name of ['DEVIN_BEARER_TOKEN', 'DEVIN_AUTHORIZATION', 'DEVIN_ORGANIZATION', 'DEVIN_ORG']) {
    for (const value of ['', '  ']) {
      const h = manual({env: {[name]: value}});
      assert.throws(h.probe, error => /missing-auth|failed/.test(error.code));
      assert.equal(h.calls.http.length + h.calls.files.length, 0);
    }
  }
  const h = manual({env: {DEVIN_BEARER_TOKEN: '', DEVIN_AUTHORIZATION: 'must-not-use'}});
  assert.throws(h.probe, error => error.code === 'missing-auth');
  assert.equal(h.calls.http.length, 0);
});

test('Devin configured cookies select web in auto mode but environment alone does not', () => {
  const configured = manual({settings: {authSource: undefined}});
  assert.equal(configured.probe().source, 'web');
  const environment = manual({settings: {authSource: undefined}, provider: {cookieHeader: undefined}, env: {DEVIN_BEARER_TOKEN: 'env'}});
  assert.throws(environment.probe, error => String(error).includes('devin auth login'));
  assert.equal(environment.calls.http.length, 0);
});

test('Devin web auth respects disabled cookies, explicit local mode, and selected CLI/IDE accounts', () => {
  for (const options of [
    {settings: {cookieSource: 'off'}}, {mode: 'local'},
    {settings: {credentialsPath: '/selected/credentials.toml'}},
    {settings: {ideVariant: 'devin'}}, {settings: {userDataDir: '/selected/ide'}},
    {mode: 'web', settings: {authSource: 'cli'}},
  ]) {
    const h = manual(options);
    assert.throws(h.probe);
    assert.equal(h.calls.http.length + h.calls.files.length, 0);
  }
});

test('Devin routes organization IDs, slugs and URLs without letting them change the request host', () => {
  for (const [organization, route] of [
    ['org-fixture', 'org-fixture'], ['organizations/org_fixture', 'org_fixture'],
    ['acme', 'org/acme'], ['org/acme', 'org/acme'],
    ['https://app.devin.ai/org/acme/settings?tab=usage', 'org/acme'],
    ['https://app.devin.ai/organizations/org_fixture', 'org_fixture'],
  ]) {
    const h = manual({provider: {workspaceId: organization}});
    h.probe();
    assert.equal(h.calls.http[0].url, `https://app.devin.ai/api/${route}/billing/quota/usage`);
  }
  for (const organization of ['../other', 'org/../other', 'org/%2e%2e', 'https://evil.example/org/acme', 'org/acme?x=y', 'org/acme\r\nX: y']) {
    const h = manual({provider: {workspaceId: organization}});
    assert.throws(h.probe, error => error.code === 'failed');
    assert.equal(h.calls.http.length, 0);
  }
});

test('Devin rejects malformed bearer headers before requesting usage', () => {
  for (const cookieHeader of ['', 'Bearer ', 'token\r\nX: y', 'not a token']) {
    const h = manual({provider: {cookieHeader}});
    assert.throws(h.probe, error => error.code === 'missing-auth');
    assert.equal(h.calls.http.length, 0);
  }
});

test('Devin only falls back between missing routes for the same organization', () => {
  let calls = 0;
  const h = manual({http: () => response(quota, ++calls === 1 ? 404 : 200)});
  assert.equal(h.probe().source, 'web');
  assert.deepEqual(h.calls.http.map(req => req.url), [
    'https://app.devin.ai/api/org_fixture/billing/quota/usage',
    'https://app.devin.ai/api/organizations/org_fixture/billing/quota/usage',
  ]);
  for (const status of [401, 403, 429, 500]) {
    const rejected = manual({http: () => response({error: 'private-response-value'}, status)});
    assert.throws(rejected.probe, error => {
      assert.doesNotMatch(error.message, /fixture-token|private-response-value/);
      return error.code === (status === 401 || status === 403 ? 'credential-denied' : 'failed');
    });
    assert.equal(rejected.calls.http.length, 1);
    assert.equal(rejected.calls.files.length, 0);
  }
  const offline = manual({http: () => { throw new Error('fixture network failure'); }});
  assert.throws(offline.probe);
  assert.equal(offline.calls.http.length, 1);
});

test('Devin preserves current percentages, known zero, legacy ratios, and missing quotas', () => {
  for (const [input, expected] of [[0, 0], [1, 1], [0.25, 25], ['12.5', 12.5], [150, 100]]) {
    const h = manual({http: () => response({daily_percentage: input})});
    assert.equal(h.probe().lines[0].used, expected);
  }
  const legacy = manual({http: () => response({usage: {daily: {used: 2, limit: 8}, weekly: {remaining_percent: 0.75}}})});
  assert.deepEqual(Array.from(legacy.probe().lines, row => row.used), [25, 25]);
  assert.equal(manual({http: () => response({daily: 0})}).probe().lines[0].resetsAt, undefined);
  for (const input of [null, false, '', 'invalid', -1e308]) {
    const h = manual({http: () => response({daily_percentage: input, daily_reset_at: '2026-10-01T00:00:00Z'})});
    assert.throws(h.probe, error => error.code === 'no-data');
    assert.equal(h.calls.http.length, 1);
  }
});
