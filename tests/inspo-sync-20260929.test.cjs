const {test} = require('node:test');
const assert = require('node:assert/strict');
const {load, metric, response} = require('./provider-sync-harness.cjs');
const {providerHarness} = require('./provider-harness.cjs');

function mistral(billing) {
  return load('mistral', {provider: {cookieHeader: 'session=fixture'}, request: req =>
    req.url.includes('/api/billing/v2/usage?') ? response(billing) : response({}, 404)}).probe();
}

test('Mistral prices event type, region and tier separately, including fine-tuning', () => {
  const row = (event_type, value, extra = {}) => ({event_type, value, billing_metric: 'shared', billing_group: 'model', ...extra});
  const rate = (event, price, extra = {}) => ({...row(event, 0, extra), price});
  const eu = {api_zone: 'eu', service_tier: 'standard'};
  const result = mistral({currency: 'EUR', currency_symbol: '€',
    completion: {models: {model: {input: [row('input_tokens', 100, {...eu, value_paid: 10})], output: [row('output_tokens', 5, eu)]}}},
    audio: {models: {model: {input: [row('seconds', 3)]}}},
    fine_tuning: {training: {model: {input: [row('training', 2)]}}, storage: {model: {input: [row('storage', 4)]}}},
    prices: [rate('input_tokens', '0.1', eu), rate('output_tokens', '0.2', eu), rate('seconds', '1'),
      rate('training', '0.3'), rate('storage', '0.05'),
      rate('input_tokens', '90', {api_zone: 'us', service_tier: 'standard'}),
      rate('input_tokens', '80', {api_zone: 'eu', service_tier: 'priority'})],
  });
  assert.equal(metric(result, 'Monthly spend').value, '€5.8000 this month (EUR)');
  assert.equal(metric(result, 'Tokens').value, '100 in / 5 out tokens');
});

test('Mistral only falls back to an unqualified price for the same event and preserves free rows', () => {
  const entry = {event_type: 'input_tokens', billing_metric: 'tokens', billing_group: 'model', api_zone: 'eu', service_tier: 'standard', value: 10};
  const price = {event_type: 'input_tokens', billing_metric: 'tokens', billing_group: 'model', price: '0'};
  const billing = {completion: {models: {model: {input: [entry]}}}, prices: [price]};
  assert.equal(metric(mistral(billing), 'Monthly spend').value, '€0.0000 this month (EUR)');
  billing.prices = [{...price, event_type: 'seconds', price: '100'}, {...price, api_zone: 'us', price: '50'}];
  const partial = metric(mistral(billing), 'Monthly spend');
  assert.equal(partial.value, '€0.0000+ this month (EUR)');
  assert.match(partial.subtitle, /no matching price/);
});

test('Z.ai distinguishes unavailable plan quotas while retaining recognized windows', () => {
  for (const limits of [[], [{type: 'NEW_LIMIT'}], [{type: 'TOKENS_LIMIT', percentage: 25, unit: 3, number: 5}, {type: 'NEW_LIMIT'}]]) {
    const result = load('zai', {provider: {apiKey: 'fixture'}, request: req => response({code: 200, data:
      req.url.includes('/quota/limit') ? {limits} : []})}).probe();
    assert.equal(metric(result, limits.length > 1 ? 'Additional quota' : 'Coding Plan usage').value, 'Unavailable');
    assert.equal(result.lines.filter(line => line.type === 'progress').length, limits.length > 1 ? 1 : 0);
    if (limits.length > 1) assert.equal(metric(result, 'Session').used, 25);
  }
});

function grouped(starter) {
  return {groups: ['Gemini Models', 'Claude and GPT models'].map((displayName, index) => ({displayName, buckets: [
    {bucketId: index + '-weekly', displayName: 'Weekly Limit', window: 'weekly', remainingFraction: 0.75},
    ...(starter ? [] : [{bucketId: index + '-5h', displayName: 'Five Hour Limit', window: '5h', remainingFraction: 0.5}]),
  ]}))};
}

const credentials = JSON.stringify({token: {access_token: 'fixture-bearer', expiry: '2999-01-01T00:00:00Z'}});
for (const provider of ['antigravity', 'antigravity-cli']) {
  for (const starter of [true, false]) test(`${provider}: preserves grouped ${starter ? 'Starter weekly-only' : 'weekly and session'} OAuth quotas`, () => {
    const app = providerHarness(provider, {host: {ls: {discoverStatus: () => ({status: 'missing'})},
      keychain: {readGenericPassword: () => credentials}}, http: req => {
      assert.equal(req.headers.Authorization, 'Bearer fixture-bearer');
      if (req.url.endsWith(':loadCodeAssist')) return response({cloudaicompanionProject: 'account-project', currentTier: {name: 'Starter'}});
      assert.ok(req.url.endsWith(':retrieveUserQuotaSummary'));
      assert.equal(req.timeoutMs, 2000);
      assert.deepEqual(JSON.parse(req.bodyText), {project: 'account-project'});
      return response({response: grouped(starter)});
    }});
    const result = app.probe();
    assert.equal(result.lines.length, starter ? 2 : 4);
    assert.equal(metric(result, 'Gemini Weekly').used, 25);
    assert.equal(metric(result, 'Claude/GPT Weekly').periodDurationMs, 7 * 24 * 60 * 60 * 1000);
    assert.equal(Boolean(metric(result, 'Gemini Session')), !starter);
    if (!starter) assert.equal(metric(result, 'Gemini Session').periodDurationMs, 5 * 60 * 60 * 1000);
    assert.equal(app.calls.http.length, 2);
  });
}

for (const provider of ['antigravity', 'antigravity-ide']) test(`${provider}: IDE uses grouped quotas even when identity metadata is unavailable`, () => {
  const app = providerHarness(provider, {host: {ls: {discoverStatus: () => ({status: 'ready', result: {ports: [12345], csrf: 'fixture'}})}},
    http: req => {
      if (req.url.endsWith('/RetrieveUserQuotaSummary')) {
        assert.equal(req.timeoutMs, 2000);
        assert.deepEqual(JSON.parse(req.bodyText), {forceRefresh: true});
        return response({summary: grouped(true)});
      }
      return response({});
    }});
  const result = app.probe();
  assert.equal(metric(result, 'Gemini Weekly').used, 25);
  assert.equal(metric(result, 'Claude/GPT Weekly').used, 25);
  assert.ok(!app.calls.http.some(req => req.url.includes('/GetCommandModelConfigs')));
});

test('Antigravity explicit cadence and unknown quota values are preserved', () => {
  const app = load('zai'); // Use the shipped shared parser without any external request.
  const lines = app.ctx.util.groupedQuotaLines({groups: [{displayName: 'Gemini Models', buckets: [
    {bucketId: 'legacy-5h', displayName: 'Five Hour Limit', window: 'weekly', remainingFraction: 1},
    {bucketId: 'opaque', displayName: 'Weekly Limit', window: 'unknown', remaining: {remainingFraction: 0.5}},
    {bucketId: 'disabled', remainingFraction: 1, disabled: true},
    {bucketId: 'missing'},
  ]}]});
  assert.equal(lines[0].periodDurationMs, 604800000);
  assert.equal(lines[1].periodDurationMs, undefined);
  assert.equal(lines[1].used, 50);
  assert.equal(lines[2].type, 'text');
  assert.equal(lines[2].value, 'Unavailable');
  assert.equal(lines[3].value, 'Unavailable');
});

test('Antigravity handles protobuf quota values and canonical cadence aliases', () => {
  const app = load('antigravity');
  const buckets = [
    {bucketId: 'oneof', window: '5-hour', remaining: {case: 'remainingFraction', value: 0.75}},
    {bucketId: 'legacy-weekly', window: ' ', remainingFraction: 0.5},
    {bucketId: 'unknown', displayName: 'Biweekly Limit', remainingFraction: 0.25},
  ];
  for (const code of [undefined, 0, '0', 'OK', 'ok', 'SUCCESS', 'success']) {
    const lines = app.ctx.util.groupedQuotaLines({code, groups: [{buckets}]});
    assert.equal(lines[0].used, 25);
    assert.equal(lines[0].periodDurationMs, 18000000);
    assert.equal(lines[1].periodDurationMs, 604800000);
    assert.equal(lines[2].periodDurationMs, undefined);
  }
  assert.deepEqual(Array.from(app.ctx.util.groupedQuotaLines({code: 403, groups: [{buckets}]})), []);
  assert.deepEqual(Array.from(app.ctx.util.groupedQuotaLines({groups: [{buckets: [
    {bucketId: 'unknown', remaining: {case: 'unlimited', value: 1}},
  ]}]})), []);
});

for (const provider of ['antigravity', 'antigravity-ide']) test(`${provider}: legacy IDE fallback omits unknown model quotas`, () => {
  const app = providerHarness(provider, {host: {ls: {discoverStatus: () => ({status: 'ready', result: {ports: [12345], csrf: 'fixture'}})}},
    http: req => req.url.endsWith('/GetUserStatus') ? response({userStatus: {cascadeModelConfigData: {clientModelConfigs: [
      {label: 'Claude'}, {label: 'Gemini Pro', quotaInfo: {remainingFraction: 0.75}},
    ]}}}) : response({})});
  const result = app.probe();
  assert.equal(result.lines.length, 1);
  assert.equal(result.lines[0].used, 25);
});

test('Antigravity summary failures fall back to per-model quotas without manufacturing exhaustion', () => {
  const app = providerHarness('antigravity', {host: {ls: {discoverStatus: () => ({status: 'missing'})},
    keychain: {readGenericPassword: () => credentials}}, http: req => {
    if (req.url.endsWith(':loadCodeAssist')) return response({});
    if (req.url.includes(':retrieveUserQuota')) return response({}, 403);
    return response({models: {unknown: {displayName: 'Claude'}, known: {displayName: 'Gemini Pro', quotaInfo: {remainingFraction: 0.75}}}});
  }});
  const result = app.probe();
  assert.equal(result.lines.length, 1);
  assert.equal(result.lines[0].used, 25);
  assert.equal(app.calls.http.filter(req => req.url.endsWith(':retrieveUserQuotaSummary')).length, 1);
  assert.ok(!app.calls.http.some(req => req.url.includes('oauth2.googleapis.com')));
});
