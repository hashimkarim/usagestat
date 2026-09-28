const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {createHash} = require('node:crypto');
const {load, metric, response, ROOT} = require('./provider-sync-harness.cjs');
const ids = 'aixy atlascloud bifrost devpass gitkraken helmcode huggingface hyper llmman muse nous raycast replicate typesafe v0 vercel xkiro'.split(' ');

test('bundled parsers have pinned provenance, licenses, opt-in manifests and valid exports', async () => {
  for (const id of ids) {
    const dir = path.join(ROOT, 'plugins', id);
    const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'plugin.json')));
    const source = fs.readFileSync(path.join(dir, 'plugin.js'), 'utf8');
    const original = source.slice(source.indexOf('\n', source.indexOf('\n') + 1) + 1);
    assert.equal(createHash('sha256').update(original).digest('hex'), manifest.upstream.sha256, id);
    assert.equal(manifest.enabledByDefault, false);
    assert.ok(fs.readFileSync(path.join(dir, 'LICENSE'), 'utf8').includes('Peter Steinberger'));
    assert.ok(manifest.supportedModes.includes(manifest.autoMode));
    assert.equal(load(id).plugin.id, id);
  }
});

for (const id of ids.filter(id => id !== 'llmman')) {
  test(`${id} never interprets missing credentials or empty data as zero quota`, async () => {
    const app = load(id, {request: () => response({})});
    await assert.rejects(app.probe);
    assert.equal(app.requests.length, 0, id);
  });
}

test('xKiro keeps free daily tokens separate from paid balances and uses the refresh clock', async () => {
  const app = load('xkiro', {now: '2026-09-27T23:59:59Z', provider: {apiKey: 'fixture'}, request: req => {
    assert.equal(req.url, 'https://api.xkiro.com/v1/usage');
    assert.equal(req.headers.Authorization, 'Bearer fixture');
    return response({object: 'usage', free_tokens: {used_today: 25, limit_per_day: 100, remaining: 75}, plan: 'Pro', balance: 99});
  }});
  const result = await app.probe();
  assert.equal(metric(result, 'Daily free tokens').used, 25);
  assert.equal(metric(result, 'Daily free tokens').resetsAt, '2026-09-28T00:00:00.000Z');
  assert.equal(metric(result, 'Daily free tokens').periodDurationMs, 86400000);
  assert.equal(result.plan, 'Pro');
  assert.equal(metric(result, 'Balance'), undefined);
});

test('Atlas preserves zero and negative USD balances without manufacturing a quota', async () => {
  for (const value of ['0', '-12.50', '25.123']) {
    const app = load('atlascloud', {provider: {apiKey: 'fixture'}, request: () => response({object: 'balance', scope: 'account', available: {currency: 'usd', value}})});
    const result = await app.probe();
    assert.equal(result.lines.some(line => line.type === 'progress'), false);
    assert.ok(metric(result, 'Available balance').value.startsWith('$'));
  }
  await assert.rejects(load('atlascloud', {provider: {apiKey: 'fixture'}, request: () => response({object: 'balance', scope: 'key', available: {currency: 'usd', value: '1'}})}).probe);
});

test('Vercel account lifetime spend is not ingested as a daily cost', async () => {
  const app = load('vercel', {provider: {apiKey: 'fixture'}, request: () => response({balance: '12.50', total_used: '4.25'})});
  const result = await app.probe();
  assert.equal(metric(result, 'Available balance').value, '$12.50');
  assert.equal(metric(result, 'Lifetime spend').value, '$4.25');
  assert.equal(app.ingested.length, 0);
});

test('DevPass separates plan, weekly and lifetime key spending', async () => {
  const app = load('devpass', {provider: {apiKey: 'fixture'}, request: () => response({data: {
    label: 'Fixture', usage: '31.42', limit: null, devPlan: 'pro', devPlanCreditsUsed: '25',
    devPlanCreditsLimit: '237', devPlanCreditsRemaining: '212.00', devPlanPremiumWeeklyLimit: '35.55',
    devPlanPremiumCreditsUsed: '5.00', devPlanPremiumWeekResetsAt: '2026-10-01T12:00:00.000Z',
  }})});
  const result = await app.probe();
  assert.equal(result.plan, 'DevPass Pro');
  assert.ok(Math.abs(metric(result, 'Plan credits').used - 25 / 237 * 100) < 1e-6);
  assert.equal(metric(result, 'Premium weekly').resetsAt, '2026-10-01T12:00:00.000Z');
  assert.equal(metric(result, 'Plan credits').resetsAt, undefined);
  assert.equal(app.ingested.length, 0);
});

test('llmman reports loaded model memory without requiring an API key', async () => {
  const app = load('llmman', {request: req => {
    assert.ok(req.url.startsWith('http://127.0.0.1:17434/'));
    assert.equal(req.headers.Authorization, undefined);
    return response(req.url.endsWith('/api/version') ? {version: '0.9.0'} : {memory: 40000000000,
      loaded: {'model/small': 2000000000, 'model/large': 8000000000}, stored: {'model/small': 2000000000}});
  }});
  assert.equal(metric(await app.probe(), 'Memory').used, 25);
});

test('Muse reuses the CLI login read-only and reports independent windows', async () => {
  const app = load('muse', {files: {'/test/home/.config/muse/auth.json': JSON.stringify({providers: {meta: {access_token: 'dca:fixture'}}})},
    request: req => {
      assert.equal(req.url, 'https://api.meta.ai/muse-code/key');
      assert.equal(req.headers.Authorization, 'Bearer dca:fixture');
      assert.equal(req.bodyText, '{}');
      return response({is_subs_active: true, subs_tier_name: 'Muse Code', subs_usage: {
        window: {used_percent: 96, window_duration_mins: 300, resets_at: 1790500000}, weekly: {used_percent: 40, resets_at: 1790800000}}});
    }});
  const result = await app.probe();
  assert.equal(metric(result, 'Session').used, 96);
  assert.equal(metric(result, 'Weekly').used, 40);
  assert.equal(app.files.size, 1);
});

test('Nous preserves subscription and top-up credit scope; never refreshes Hermes tokens', async () => {
  const app = load('nous', {files: {'/test/home/.hermes/auth.json': JSON.stringify({providers: {nous: {access_token: 'fixture', portal_base_url: 'https://attacker.example'}}})},
    request: req => {
      assert.equal(req.url, 'https://portal.nousresearch.com/api/oauth/account');
      assert.equal(req.headers.Authorization, 'Bearer fixture');
      return response({subscription: {plan: 'Ultra', monthly_credits: 220, credits_remaining: 55,
        current_period_end: '2026-10-12T04:29:00Z'}, purchased_credits_remaining: 19.25});
    }});
  const result = await app.probe();
  assert.equal(metric(result, 'Monthly credits').used, 75);
  assert.equal(metric(result, 'Top-up credits').value, '$19.25');
  assert.equal(app.files.size, 1);
});

test('named accounts cannot inherit ambient credentials, cookies or local CLI logins', async () => {
  for (const id of ['atlascloud', 'hyper', 'muse', 'nous', 'raycast']) {
    const app = load(id, {provider: {instanceId: id + '-other'}, env: {ATLASCLOUD_API_KEY: 'ambient', HYPER_API_KEY: 'ambient', RAYCAST_COOKIE: 'session=ambient'},
      files: {'/test/home/.hermes/auth.json': '{"access_token":"ambient"}', '/test/home/.config/muse/auth.json': '{"providers":{"meta":{"access_token":"dca:ambient"}}}'}});
    await assert.rejects(app.probe);
    assert.equal(app.requests.length, 0, id);
  }
});

test('HTTP failures neither leak bodies/credentials nor retry a rate limit', async () => {
  for (const status of [401, 403, 429, 500]) {
    const app = load('atlascloud', {provider: {apiKey: 'secret-fixture'}, request: () => response('private-body', status)});
    await assert.rejects(app.probe, error => !/secret-fixture|private-body/.test(error.message));
    assert.equal(app.requests.length, 1);
  }
});

test('adapter blocks off-origin requests before attaching provider credentials', async () => {
  const script = fs.readFileSync(path.join(ROOT, 'crates/ai-usage-plugins/src/bundled_provider.js'), 'utf8');
  const app = load('atlascloud', {provider: {apiKey: 'secret-fixture'}});
  const sandbox = vm.createContext({});
  vm.runInContext(script + `\ndefineProvider({id:'test', name:'Test', endpoints:['https://good.example'],
    auth:{type:'bearer',secret:'TOKEN'}, settings:[{key:'TOKEN',type:'secure'}],
    async fetchUsage(ctx){return await ctx.http.get('https://evil.example/usage')}});`, sandbox);
  await assert.rejects(sandbox.__usagestat_plugin.probe(app.ctx), /outside/);
  assert.equal(app.requests.length, 0);
});

test('Helmcode separates monthly and premium windows and keeps optional credit failure non-fatal',async()=>{
  const app=load('helmcode',{provider:{cookieHeader:'session=fixture'},request:req=>{
    if(req.url.endsWith('/api/billing'))return response({subscription:{premium:true}});
    if(req.url.endsWith('/credits'))return response({},503);
    return response({periodStart:'2026-09-01',models:[{model:'Alpha',cap:100,tokensUsed:25},{model:'Beta',cap:50,tokensUsed:30,windowHours:5}]});
  }});
  const result=await app.probe();
  assert.equal(result.source,'web');
  assert.equal(metric(result,'Model quota').used,60);
  assert.equal(metric(result,'Alpha').used,25);
});

test('GitKraken separates personal and shared quota',async()=>{
  const app=load('gitkraken',{provider:{apiKey:'fixture',workspaceId:'org'},request:req=>{
    assert.equal(req.headers['gk-org-id'],'org');
    return response({data:{used:25,limit:100,resetsOn:'2026-10-01T00:00:00Z',sharedPool:{used:40,limit:200}}});
  }});
  assert.equal(metric(await app.probe(),'Personal').used,25);
});

test('Hugging Face bills net inference usage without turning a spend cap into a quota',async()=>{
  const app=load('huggingface',{provider:{apiKey:'fixture'},request:req=>req.url.includes('usage-v2')
    ? response({usage:{inferenceProviders:{usedNanoUsd:2450000000,includedNanoUsd:2000000000,limitNanoUsd:4000000000,numRequests:128}}})
    : response({},503)});
  const result=await app.probe();
  assert.ok(result.lines.every(line=>line.type!=='progress'));
  assert.equal(metric(result,'Billable usage').value,'$0.45');
});

test('Hyper reports a credit balance without synthesizing quota',async()=>{
  const app=load('hyper',{provider:{apiKey:'fixture'},request:()=>response({balance:12.5})});
  const result=await app.probe();
  assert.equal(metric(result,'Balance').value,'12.5 HC');
  assert.ok(result.lines.every(line=>line.type!=='progress'));
});

test('Raycast sends only its session cookies and retains the renewal',async()=>{
  const app=load('raycast',{provider:{cookieHeader:'__raycast_session=fixture; csrf_token=csrf; unrelated=private'},request:req=>{
    assert.equal(req.headers.Cookie,'__raycast_session=fixture; csrf_token=csrf');
    return response({remaining_balance_credits:75,total_balance_credits:100,next_credits_at:'2026-10-01T00:00:00Z',funding_subscription:{tier:'pro_plus'}});
  }});
  const result=await app.probe();
  assert.equal(result.source,'web');
  assert.equal(result.plan,'Pro+');
  assert.equal(metric(result,'Credits').used,25);
});

test('Replicate uses the selected billing identity and preserves invoice spend',async()=>{
  const app=load('replicate',{provider:{cookieHeader:'session=fixture'},request:req=>{
    if(req.url.endsWith('/billing'))return response('<script type="application/json" id="react-component-props-billing-page">{"page":{"account":{"kind":"user","username":"fixture"}}}</script>');
    if(req.url.includes('invoices'))return response({invoices:[{type:'monthly-usage',ended_before:null,total_cost_before_adjustments:'12.40',total_cost:'0'}]});
    return response({unused_credit:'80'});
  }});
  const result=await app.probe();
  assert.ok(result.lines.some(line=>line.value==='$12.40'));
  assert.ok(result.lines.every(line=>line.type!=='progress'));
});

test('TypeSafe discovers its billing action and posts structured JSON',async()=>{
  const action='b'.repeat(40);
  const app=load('typesafe',{provider:{cookieHeader:'session=fixture'},request:req=>{
    if(req.method==='POST'){
      assert.equal(req.headers['Next-Action'],action);
      assert.equal(req.bodyText,'[]');
      return response('0:{"a":"$@1"}\n1:{"ok":true,"data":{"billing":{"plan":"free_plan","spent":0.01,"freeCreditsRemaining":4.98,"balance":4.98,"purchased":0,"cycleLabel":"September 2026","credits":[]}}}');
    }
    if(req.url.includes('/_next/'))return response('"'+action+'",c.callServer,void 0,c.findSourceMapURL,"getBillingOverviewResult"');
    return response('<script src="/_next/static/chunks/app.js"></script>');
  }});
  assert.equal(metric(await app.probe(),'Balance').value,'USD 4.98');
});

test('v0 keeps billing and rate limits independent',async()=>{
  const app=load('v0',{provider:{apiKey:'fixture'},request:req=>response(req.url.includes('/user/billing')
    ? {billingType:'token',data:{balance:{total:100,remaining:80},billingCycle:{end:1790800000}}}
    : {limit:50,remaining:45,reset:1790600000})});
  const result=await app.probe();
  assert.equal(metric(result,'Billing').used,20);
  assert.equal(metric(result,'Rate limit').used,10);
});

test('Bifrost preserves monthly budget and scoped rate limits',async()=>{
  const app=load('bifrost',{provider:{apiKey:'fixture',settings:{BIFROST_BASE_URL:'http://127.0.0.1:8080'}},request:()=>response({
    virtual_key_name:'fixture',budgets:[{id:'month',max_limit:125,current_usage:42.17,reset_duration:'1M'}]})});
  assert.ok(Math.abs(metric(await app.probe(),'Budget').used-33.736)<1e-8);
});

test('Aixy preserves observed key identity without inventing absent budgets',async()=>{
  const app=load('aixy',{provider:{apiKey:'fixture'},request:()=>response({object:'key.usage',currency:'USD',as_of:'2026-09-05T12:00:00Z',
    key:{id:'key_fixture',project_id:'project_fixture'},budgets:[]})});
  const result=await app.probe();
  assert.equal(metric(result,'Key').value,'key_fixture');
  assert.ok(result.lines.every(line=>line.type!=='progress'));
});
