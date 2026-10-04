const {test} = require('node:test');
const assert = require('node:assert/strict');
const {load, metric, response} = require('./provider-sync-harness.cjs');
const credentials = {accessToken: 'fixture', expiresAt: 1999999999999, scopes: ['user:profile'], subscriptionType: 'pro'};
const quota = {five_hour: {utilization: 10}, seven_day: {utilization: 25}};

test('OpenCode Go keeps token-only messages without estimating a false quota',()=>{
  const app=load('opencode-go',{source:'local',sqlite:(db,sql)=>JSON.stringify(sql.includes('sqlite_master')?[{name:'message'}]:[
    {createdMs:Date.parse('2026-09-05T10:00:00Z'),cost:null,inputTokens:10,outputTokens:5,reasoningOutputTokens:3},
    {createdMs:Date.parse('2026-09-05T11:00:00Z'),cost:1.2,inputTokens:20,outputTokens:10},
    {createdMs:Date.parse('2026-09-06T10:00:00Z'),cost:999,inputTokens:999},
  ])});
  const result=app.probe(), day=app.ingested[0].daily[0];
  assert.equal(day.totalTokens,45);
  assert.equal(day.costUsd,1.2);
  assert.equal(day.tokensKnown,true);
  assert.equal(day.costKnown,false);
  assert.ok(result.lines.every(line=>line.type!=='progress'));
  assert.equal(metric(result,'Local Cost').value,'$1.20+');
});

const envelope = data => response({code:0,data:{biz_code:0,biz_data:data}});
test('DeepSeek Platform reports per-model daily spend without treating balance as quota',()=>{
  const time=Date.parse('2026-09-05T00:00:00Z')/1000;
  const app=load('deepseek',{source:'web',provider:{cookieHeader:'platform-fixture'},request:req=>{
    assert.equal(req.headers.Authorization,'Bearer platform-fixture');
    assert.ok(req.url.startsWith('https://platform.deepseek.com/'));
    if(req.url.endsWith('get_user_summary'))return envelope({normal_wallets:[{currency:'USD',balance:5}],bonus_wallets:[{currency:'USD',balance:2}]});
    if(req.url.includes('/amount?'))return envelope({series:[{api_key:'one',model:'reasoner',buckets:[{time,usage:{PROMPT_CACHE_MISS_TOKEN:100,PROMPT_CACHE_HIT_TOKEN:20,RESPONSE_TOKEN:30,REQUEST:2}}]}]});
    return envelope({data:[{currency:'USD',series:[{api_key:'one',model:'reasoner',buckets:[{time,cost:0.25}]}]}]});
  }});
  const result=app.probe();
  assert.equal(metric(result,'Balance').value,'USD 7.00');
  assert.equal(metric(result,'reasoner').value,'USD 0.25');
  assert.equal(app.ingested[0].daily[0].totalTokens,150);
  assert.equal(app.ingested[0].daily[0].costKnown,true);
  assert.ok(result.lines.every(line=>line.type!=='progress'));
  assert.equal(app.requests.length,3);
});

test('DeepSeek keeps API balance isolated from ambient website accounts and prefers funded currency',()=>{
  const app=load('deepseek',{provider:{apiKey:'api-fixture'},env:{DEEPSEEK_PLATFORM_TOKEN:'different-account'},request:req=>{
    assert.equal(req.headers.Authorization,'Bearer api-fixture');
    return response({balance_infos:[{currency:'USD',total_balance:0,granted_balance:0,topped_up_balance:0},
      {currency:'CNY',total_balance:20,granted_balance:0,topped_up_balance:20}]});
  }});
  assert.equal(metric(app.probe(),'Balance').value,'CNY 20.00');
  assert.equal(app.requests.length,1);
  const scoped=load('deepseek',{provider:{instanceId:'work'},env:{DEEPSEEK_API_KEY:'ambient',DEEPSEEK_PLATFORM_TOKEN:'ambient'}});
  assert.throws(scoped.probe,error=>error.code==='missing-auth');
});

test('DeepSeek monthly fallback preserves currency and does not label CNY history as USD',()=>{
  const app=load('deepseek',{source:'web',provider:{cookieHeader:'fixture'},request:req=>{
    if(req.url.endsWith('get_user_summary'))return envelope({normal_wallets:[{currency:'CNY',balance:5}],bonus_wallets:[]});
    if(req.url.includes('/by_api_key/'))return response({},404);
    if(req.url.includes('/amount?'))return envelope({days:[{date:'2026-09-05',data:[{model:'chat',usage:[{type:'RESPONSE_TOKEN',amount:'15'}]}]}]});
    return envelope([{currency:'CNY',days:[{date:'2026-09-05',data:[{model:'chat',usage:[{type:'RESPONSE_TOKEN',amount:'0.5'}]}]}]}]);
  }});
  assert.equal(metric(app.probe(),'This month').value,'CNY 0.50');
  assert.equal(app.ingested[0].daily[0].costKnown,false);
  assert.equal(app.ingested[0].daily[0].costUsd,0);
  assert.equal(app.ingested[0].daily[0].totalTokens,15);
});

test('CodeRabbit uses one bounded usage report and never invents a limit', () => {
  let calls = 0;
  const app = load('coderabbit', {command: req => {
    calls++;
    assert.equal(req.program, 'coderabbit');
    assert.equal(JSON.stringify(req.args), '["usage"]');
    assert.equal(req.timeoutMs, 15000);
    return {status: 0, stdout: '\u001b[32mPlan: Pro\u001b[0m\nYour reviews: 0\nUsage billing: Active\nOrganization: Fixture', stderr: ''};
  }});
  const result = app.probe();
  assert.equal(result.plan, 'Pro');
  assert.equal(metric(result, 'Reviews').value, '0');
  assert.ok(result.lines.every(line => line.type !== 'progress'));
  assert.equal(calls, 1);
});

test('ClinePass reuses the selected browser sign-in file read-only', () => {
  const app = load('clinepass', {files: {'/test/home/.cline/data/settings/providers.json': JSON.stringify({providers: {
    cline: {settings: {apiKey: 'old', auth: {accessToken: 'fixture'}}}}})}, request: req => {
    assert.equal(req.headers.Authorization, 'Bearer workos:fixture');
    return response({success: true, data: {limits: [{type: 'five_hour', percentUsed: 25}]}});
  }});
  assert.equal(app.probe().source, 'oauth');
  assert.equal(app.files.size, 1);
  assert.equal(app.requests.length, 1);
});

test('Claude refreshes plan from the live profile once per credential across restarts', () => {
  const first = load('claude', {credentials, request: req => req.url.endsWith('/profile')
    ? response({organization: {organization_type: 'claude_max', rate_limit_tier: 'default_claude_max_20x'}}) : response(quota)});
  assert.equal(first.probe().plan, 'Max 20x');
  assert.ok(first.requests[0].url.endsWith('?cedar_ember=1&skip_spend=1'));
  const second = load('claude', {credentials, now: '2026-09-05T12:06:00Z', files: Object.fromEntries(first.files), request: req => {
    assert.ok(!req.url.endsWith('/profile'));
    return response(quota);
  }});
  assert.equal(second.probe().plan, 'Max 20x');
  assert.equal(second.requests.length, 1);
});

test('Claude profile failure neither breaks quotas nor repeats within the same credential', () => {
  const first = load('claude', {credentials, request: req => req.url.endsWith('/profile') ? response({}, 503) : response(quota)});
  assert.equal(metric(first.probe(), 'Session').used, 10);
  const second = load('claude', {credentials, now: '2026-09-05T12:06:00Z', files: Object.fromEntries(first.files), request: () => response(quota)});
  second.probe();
  assert.equal(second.requests.length, 1);
});

test('Claude reset grants are read-only, exclude expired grants and preserve paused balances', () => {
  const app = load('claude', {credentials, request: req => req.url.endsWith('/profile') ? response({}, 503) : response({...quota,
    cedar_ember: {eligible: true, grants: [
      {resets_left: 2, ends_at: '2026-09-30T00:00:00Z', paused: true},
      {resets_left: 9, ends_at: '2026-08-30T00:00:00Z'}, {resets_left: null}, {resets_left: false},
    ]}})});
  const result = app.probe();
  assert.equal(metric(result, 'Rate Limit Resets').value, '2 available');
  assert.match(metric(result, 'Rate Limit Resets').subtitle, /2026-09-30/);
  assert.ok(app.requests.every(req => req.method === 'GET'));
});

function gemini(options = {}) {
  const rpc = (id, value) => ")]}'\n" + JSON.stringify([['wrb.fr', id, JSON.stringify(value), null]]);
  return load('gemini-apps', {provider: {cookieHeader: '__Secure-1PSID=fixture; SAPISID=fixture'}, request: req => {
    assert.match(req.headers.Authorization, /^SAPISIDHASH \d+_[a-f0-9]{40}$/);
    if (req.method === 'GET') return response('<script>{"SNlM0e":"csrf","cfb2h":"build","FdrFJe":"session"}</script>');
    if (req.url.includes('rpcids=jSf9Qc')) return response(rpc('jSf9Qc', [2, [[1, options.current ?? 0.13, 1, [[1790500000]]], [2, 0.2656, 2, [[1790800000]]]]]));
    return response(rpc('sJBwce', ['Pro']));
  }});
}

test('Gemini Apps web usage stays distinct from Gemini CLI and Antigravity allowances', () => {
  const app = gemini();
  const result = app.probe();
  assert.equal(metric(result, 'Current').used, 13);
  assert.equal(metric(result, 'Weekly').used, 27);
  assert.equal(result.plan, 'Pro');
  assert.equal(result.source, 'web');
  assert.ok(app.requests.every(req => new URL(req.url).hostname === 'gemini.google.com'));
});

test('Gemini Apps rejects malformed quota values and does not inherit another account cookie', () => {
  assert.throws(gemini({current: false}).probe, /parse/);
  const app = load('gemini-apps', {provider: {instanceId: 'other'}, env: {GEMINI_COOKIE: '__Secure-1PSID=ambient; SAPISID=ambient'}});
  assert.throws(app.probe, /auth missing/);
  assert.equal(app.requests.length, 0);
});

test('Pi deduplicates mirrored sessions, counts cache once and retains unpriced models', () => {
  const session = {type:'session',id:'same'};
  const entry = {type:'message',id:'turn-1',timestamp:'2026-09-05T10:00:00Z',message:{role:'assistant',provider:'openai-codex',
    model:'gpt-5.4',usage:{input:180000,output:10,cacheRead:60000,cacheWrite:0}}};
  const known = [session,entry].map(JSON.stringify).join('\n');
  const unknown = JSON.stringify({...entry,id:'turn-2',message:{...entry.message,model:'new-unpriced-model',usage:{input:20,output:5}}});
  const app = load('pi',{files:{'/test/home/.pi/agent/sessions/project/a.jsonl':known,
    '/test/home/.omp/agent/sessions/project/a.jsonl':known+'\n'+unknown}});
  const result = app.probe();
  assert.equal(result.source,'local');
  assert.ok(result.lines.every(line=>line.type!=='progress'));
  const day=app.ingested[0].daily[0];
  assert.equal(day.totalTokens,240035);
  assert.equal(day.requests,2);
  assert.equal(day.tokensKnown,true);
  assert.equal(day.costKnown,false);
  assert.ok(Math.abs(day.costUsd-.46515)<1e-8);
});

test('Pi explicit roots and named profiles do not merge ambient account history', () => {
  const entry=JSON.stringify({type:'message',timestamp:'2026-09-05T10:00:00Z',message:{role:'assistant',provider:'anthropic',model:'unknown',usage:{input:4,output:2}}});
  const app=load('pi',{env:{OMP_PROFILE:'work'},files:{'/test/home/.pi/agent/sessions/a.jsonl':entry,
    '/test/home/.omp/profiles/work/sessions/a.jsonl':entry}});
  app.probe();
  assert.equal(app.ingested[0].daily[0].totalTokens,6);
  const scoped=load('pi',{provider:{instanceId:'other'}});
  assert.throws(scoped.probe,/sessionRoots/);
});

test('Command Code reported grant wins over stale plan catalogue and lifetime spend', () => {
  const app=load('command-code',{provider:{apiKey:'fixture'},request:req=>{
    if(req.url.endsWith('/credits')) return response({credits:{monthlyCredits:75,monthlyCreditsGranted:100}});
    if(req.url.endsWith('/summary')) return response({totalMonthlyCredits:950});
    if(req.url.endsWith('/subscriptions')) return response({success:true,data:{planId:'individual-go'}});
    return response({id:'fixture'});
  }});
  assert.equal(metric(app.probe(),'Monthly credits').used,25);
});

test('Mistral counts plan-covered API, chat and Vibe tokens independently of billed units', () => {
  const models={test:{input:[{billing_metric:'in',billing_group:'test',value:100,value_paid:0}],output:[]}};
  const app=load('mistral',{provider:{cookieHeader:'ory_session_test=fixture'},request:()=>response({currency:'EUR',currency_symbol:'EUR ',prices:[],
    completion:{models},chat:{models},vibe_code:{completion:{models}}})});
  assert.equal(metric(app.probe(),'Tokens').value,'300 in / 0 out tokens');
});

test('Mistral parses included budgets across Flight chunks and ignores text-record decoys',()=>{
  const budget={budget:{api_budget:{usage_percentage:25,initial_budget:100,currency:'EUR',reset_at:'2026-10-01T00:00:00Z'},
    vibe_budget:{usage_percentage:10,initial_budget:50,currency:'EUR'}}};
  const decoy='1:'+JSON.stringify({budget:{api_budget:{usage_percentage:99,initial_budget:99,currency:'EUR'}}})+'\n';
  const stream='0:T'+Buffer.byteLength(decoy).toString(16)+','+decoy+'2:'+JSON.stringify(budget)+'\n';
  const html=[stream.slice(0,29),stream.slice(29)].map(chunk=>'self.__next_f.push('+JSON.stringify([1,chunk])+')').join('');
  const app=load('mistral',{provider:{cookieHeader:'ory_session_x=fixture; csrftoken=csrf; private=keep'},request:req=>{
    if(req.url.endsWith('/subscription'))return response(html);
    if(req.url.endsWith('/credits'))return response({wallet_amount:20,credit_notes_amount:2,ongoing_usage_balance:5,currency:'EUR'});
    return response({completion:{models:{}},currency:'EUR'});
  }});
  const result=app.probe();
  assert.equal(metric(result,'Included API').used,25);
  assert.equal(metric(result,'Monthly Plan').used,10);
  assert.equal(metric(result,'Credit balance').value,'EUR 17.00');
  assert.equal(app.requests.length,3);
});

test('Mistral Vibe fallback keeps admin-only cookies on their origin',()=>{
  const app=load('mistral',{provider:{cookieHeader:'ory_session_x=fixture; csrftoken=csrf; private=keep'},request:req=>{
    if(req.url.includes('console.mistral.ai')){
      assert.equal(req.headers.Cookie,'ory_session_x=fixture; csrftoken=csrf');
      return response([{result:{data:{json:{usage_percentage:40,reset_at:'2026-10-01T00:00:00Z'}}}}]);
    }
    if(req.url.includes('/v2/usage'))return response({completion:{models:{}}});
    return response({},503);
  }});
  assert.equal(metric(app.probe(),'Monthly Plan').used,40);
});

test('Venice Clerk sessions use bearer-only transport and banked credits are not a quota',()=>{
  const claims={exp:1790800000,userType:'pro',bundledCredits:60,veniceCredits:90,
    bundledCreditsUsage:{usedThisCycle:120,monthlyRefillCredits:100,tierCap:200,nextRefillAt:1790800000000}};
  const token='e30.'+Buffer.from(JSON.stringify(claims)).toString('base64url')+'.fixture';
  const app=load('venice',{source:'web',provider:{cookieHeader:'__session=clerk; __client=private; unrelated=private'},request:req=>{
    assert.equal(req.headers.Cookie,undefined);
    assert.equal(req.headers.Authorization,'Bearer clerk');
    return response({token});
  }});
  const result=app.probe();
  assert.equal(result.source,'web');
  assert.equal(metric(result,'Subscription credits available').value,'60');
  assert.ok(result.lines.every(line=>line.type!=='progress'));
});

test('Venice legacy chunks win over Clerk and disabled cookies send no request',()=>{
  const app=load('venice',{source:'web',provider:{cookieHeader:'__venice-auth.session-token.1=two; __session=clerk; __venice-auth.session-token.0=one'},request:req=>{
    assert.equal(req.headers.Cookie,'__venice-auth.session-token=onetwo');
    assert.equal(req.headers.Authorization,undefined);return response({},401);
  }});
  assert.throws(app.probe,/expired/);
  const disabled=load('venice',{source:'web',provider:{cookieHeader:'__session=clerk',settings:{cookies:'off'}}});
  assert.throws(disabled.probe,/disabled/);assert.equal(disabled.requests.length,0);
});

test('LiteLLM falls back to calendar month spend without inventing a budget',()=>{
  const app=load('litellm',{provider:{apiKey:'fixture',settings:{baseURL:'https://proxy.example'}},request:req=>{
    if(req.url.endsWith('/key/info'))return response({},403);
    assert.match(req.url,/start_date=2026-09-01&end_date=2026-09-05/);return response([{total_cost:2},{total_cost:3}]);
  }});
  assert.equal(metric(app.probe(),'Month to date').value,'$5.00');
});

test('LiteLLM optional paginated model activity never removes a valid budget',()=>{
  for(const bad of [false,true]){
    const app=load('litellm',{provider:{apiKey:'fixture',settings:{baseURL:'https://proxy.example',modelUsage:true}},request:req=>{
      if(req.url.endsWith('/key/info'))return response({info:{user_id:'u'}});
      if(req.url.includes('/user/info'))return response({user_info:{user_id:'u',spend:25,max_budget:100}});
      return response({results:[{date:'2026-09-05',breakdown:{models:{fixture:{prompt_tokens:10,completion_tokens:5,total_tokens:bad?null:15,api_requests:1}}}}]});
    }});
    const result=app.probe();assert.equal(metric(result,'Personal budget').used,25);
    assert.equal(!!metric(result,'fixture'),!bad);
  }
});
