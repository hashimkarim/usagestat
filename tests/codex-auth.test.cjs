const {test} = require('node:test');
const assert = require('node:assert/strict');
const {providerHarness} = require('./provider-harness.cjs');

function state(storage, stale = false) {
  return {source: 'native', storage, profileKey: 'fixture-profile', revision: 'fixture-revision', readOnly: storage === 'encrypted',
    auth: {tokens: {access_token: 'synthetic-access', refresh_token: 'synthetic-refresh', account_id: 'fixture-account'},
      last_refresh: stale ? '2025-01-01T00:00:00Z' : new Date().toISOString()}};
}
const usage = {plan_type: 'plus', rate_limit: {primary_window: {used_percent: 25}, secondary_window: {used_percent: 40}}};
const response = (status, body) => ({status, headers: {}, bodyText: JSON.stringify(body)});

for (const platform of ['linux', 'macos', 'windows']) {
  test(`${platform}: Codex uses only the host-selected profile and auth store`, () => {
    const reads = [];
    const harness = providerHarness('codex', {platform, settings: {authStorage: 'keyring'}, host: {
      codex: {readAuth(selected) { reads.push(selected); return JSON.stringify(state('keyring')); }, writeAuth() { assert.fail('unexpected refresh'); }},
    }, http: () => response(200, usage)});
    assert.equal(harness.probe().lines[0].used, 25);
    assert.deepEqual(reads, ['keyring']);
    assert.equal(harness.calls.files.length, 0);
    assert.equal(harness.calls.http.length, 1);
    assert.equal(harness.calls.http[0].headers.Authorization, 'Bearer synthetic-access');
  });
  test(`${platform}: expired encrypted Codex credentials never rotate a token without writeback`, () => {
    const harness = providerHarness('codex', {platform, host: {codex: {
      readAuth: () => JSON.stringify(state('encrypted', true)), writeAuth() { assert.fail('encrypted store mutation'); },
    }}, http(request) { assert.equal(request.method, 'GET'); return response(401, {}); }});
    assert.throws(() => harness.probe(), error => /Session expired/.test(String(error)));
    assert.equal(harness.calls.http.length, 1);
    assert.equal(harness.calls.writes.length, 0);
  });
}

test('Codex preserves native credential denied/missing/malformed states before any request', () => {
  for (const code of ['credential-denied', 'credential-missing', 'credential-malformed', 'credential-account-mismatch']) {
    const harness = providerHarness('codex', {platform: 'windows', host: {codex: {readAuth() { throw new Error(code + ': fixture'); }}}});
    assert.throws(() => harness.probe(), error => error.message === code + ': fixture');
    assert.equal(harness.calls.http.length, 0);
    assert.equal(harness.calls.files.length, 0);
  }
});

test('Codex refresh writes only the original selected native profile and revision', () => {
  const writes = [];
  const harness = providerHarness('codex', {host: {codex: {
    readAuth: () => JSON.stringify(state('keyring', true)), writeAuth: (...args) => writes.push(args),
  }}, http: request => request.method === 'POST'
    ? response(200, {access_token: 'synthetic-updated', refresh_token: 'synthetic-updated-refresh'}) : response(200, usage)});
  assert.equal(harness.probe().lines[0].used, 25);
  assert.equal(writes.length, 1);
  assert.deepEqual(writes[0].slice(0, 4), ['auto', 'fixture-profile', 'fixture-revision', 'keyring']);
  assert.equal(JSON.parse(writes[0][4]).tokens.account_id, 'fixture-account');
});

for (const platform of ['linux', 'macos', 'windows']) {
  test(`${platform}: Codex permission denial never refreshes or changes credentials`, () => {
    const harness = providerHarness('codex', {platform, host: {codex: {
      readAuth: () => JSON.stringify(state('keyring')),
      writeAuth() { assert.fail('permission denial must not rotate credentials'); },
    }}, http(request) {
      assert.equal(request.method, 'GET');
      return response(403, {});
    }});
    assert.throws(() => harness.probe(), error => /access denied.*403/.test(String(error)));
    assert.equal(harness.calls.http.length, 1);
  });
}

test('Codex preserves a permission denial after reloading changed CLI credentials', () => {
  let reads = 0;
  let requests = 0;
  const harness = providerHarness('codex', {host: {codex: {
    readAuth() {
      const selected = state('file');
      if (++reads > 1) {
        selected.revision = 'changed-revision';
        selected.auth.tokens.access_token = 'changed-access';
      }
      return JSON.stringify(selected);
    },
    writeAuth() { assert.fail('permission denial must not rotate credentials'); },
  }}, http(request) {
    assert.equal(request.method, 'GET');
    return response(++requests === 1 ? 401 : 403, {});
  }});
  assert.throws(() => harness.probe(), error => /access denied.*403/.test(String(error)));
  assert.equal(requests, 2);
});
