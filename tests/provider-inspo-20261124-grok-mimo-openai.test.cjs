const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {load,metric,response,ROOT}=require('./provider-sync-harness.cjs');

function grok(config) {
 return load('grok',{env:{GROK_HOME:'/test/grok'},files:{'/test/grok/auth.json':JSON.stringify({session:{key:'token'}})},
  request:req=>req.url.includes('/billing')?response({config}):response({subscription_tier_display:'SuperGrok'})});
}
const weekly={type:'USAGE_PERIOD_TYPE_WEEKLY',start:'2026-09-03T00:00:00Z',end:'2026-09-10T00:00:00Z'};

test('Grok reports a prepaid balance without quota fields as a valid balance-only snapshot',()=>{
 const result=grok({prepaidBalance:{val:1446}}).probe();
 assert.equal(metric(result,'Balance').value,'$14.46');
 assert.equal(result.lines.length,1);
});

test('Grok keeps the weekly quota and adds the prepaid balance beside it',()=>{
 // Live credits payload shape from upstream GrokCreditsProxyFetcherTests.
 const result=grok({currentPeriod:weekly,creditUsagePercent:100,onDemandCap:{val:0},onDemandUsed:{val:0},prepaidBalance:{val:'1761'}}).probe();
 assert.equal(metric(result,'Weekly limit').used,100);
 assert.equal(metric(result,'Balance').value,'$17.61');
 const legacy=grok({used:{val:5},monthlyLimit:{val:10},onDemandCap:{val:0},billingPeriodEnd:'2026-09-30T00:00:00Z',prepaidBalance:{val:5}}).probe();
 assert.equal(metric(legacy,'Credits used').used,50);
 assert.equal(metric(legacy,'Balance').value,'$0.05');
});

test('Grok treats an empty Cent object as zero and malformed wallets as absent',()=>{
 assert.equal(metric(grok({prepaidBalance:{}}).probe(),'Balance').value,'$0.00');
 for(const prepaidBalance of [null,{val:-1},{val:1.5},{val:'abc'},{other:1},[]]) {
  const result=grok({currentPeriod:weekly,creditUsagePercent:10,prepaidBalance}).probe();
  assert.equal(metric(result,'Balance'),undefined);assert.equal(metric(result,'Weekly limit').used,10);
  assert.throws(grok({prepaidBalance}).probe);
 }
 assert.throws(grok({}).probe);
});

// Documented at mimo.mi.com/docs/en-US/api/asset/credits-get (upstream MiMoTokenPlanPluginTests fixture).
const mimoFixture={object:'token_plan',plan_code:'lite',
 period:{start_at:'2026-11-11 21:05:27',end_at:'2026-12-11 23:59:59'},
 quota:{total_credits:4100000000,used_credits:1400000000,remaining_credits:2700000000},
 auto_renew:true,claw_enabled:false};
function mimo(options={}) {
 const region=options.region;
 return load('mimo',{source:options.source||'auto',env:{MIMO_API_KEY:'tp-fixture',...(region?{MIMO_TOKEN_PLAN_REGION:region}:{}),...options.env},
  provider:options.provider||{},request:req=>{
   assert.equal(req.url,'https://token-plan-'+(region||'cn')+'.xiaomimimo.com/v1/platform/token_plan');
   assert.equal(req.method,'GET');assert.equal(req.headers.Authorization,'Bearer tp-fixture');
   assert.equal(req.headers.Cookie,undefined);
   return response(options.body===undefined?mimoFixture:options.body,options.status||200);
  }});
}

test('MiMo Token Plan key reads exact credits from the default China endpoint',()=>{
 const app=mimo();const result=app.probe();
 const credits=metric(result,'Credits');
 assert.equal(credits.used,1400000000);assert.equal(credits.limit,4100000000);
 assert.equal(credits.resetsAt,'2026-12-11T23:59:59.000Z');
 assert.equal(credits.periodDurationMs,undefined);
 assert.equal(result.plan,'lite');assert.equal(result.source,'api');
 assert.equal(metric(result,'Balance'),undefined);assert.equal(app.requests.length,1);
});

test('MiMo uses the Singapore endpoint when the region is sgp',()=>{
 assert.equal(metric(mimo({region:'sgp'}).probe(),'Credits').used,1400000000);
 assert.throws(load('mimo',{env:{MIMO_API_KEY:'tp-fixture',MIMO_TOKEN_PLAN_REGION:'us'}}).probe,/region/);
});

test('MiMo API errors fail the selected source and never fall back to the browser session',()=>{
 for(const status of [401,403]) assert.throws(mimo({status,env:{MIMO_COOKIE:'api-platform_serviceToken=t; userId=1'}}).probe,
  error=>error.code==='credential-denied'&&/API key/.test(error.message));
 assert.throws(mimo({status:429}).probe,/rate limiting/);
 assert.throws(mimo({status:500}).probe,/HTTP 500/);
 const exhausted={...mimoFixture,quota:{total_credits:100,used_credits:100,remaining_credits:0}};
 assert.equal(metric(mimo({body:exhausted}).probe(),'Credits').used,100);
 for(const body of ['{}','[]','not-json',
  {...mimoFixture,quota:{...mimoFixture.quota,total_credits:-1}},
  {...mimoFixture,quota:{...mimoFixture.quota,used_credits:true}},
  {...mimoFixture,quota:{...mimoFixture.quota,remaining_credits:0}},
  {...mimoFixture,period:{...mimoFixture.period,end_at:'2026-02-30 23:59:59'}},
 ]) assert.throws(mimo({body}).probe,/unrecognized/);
 assert.throws(load('mimo',{source:'api'}).probe,error=>error.code==='missing-auth');
});

test('MiMo without a key, or in web mode, still uses the console cookie path',()=>{
 for(const [source,env] of [['auto',{}],['web',{MIMO_API_KEY:'tp-fixture'}]]) {
  const app=load('mimo',{source,env:{...env,MIMO_COOKIE:'api-platform_serviceToken=t; userId=1'},request:req=>{
   assert.ok(req.url.startsWith('https://platform.xiaomimimo.com/api/v1/'));assert.equal(req.headers.Authorization,undefined);
   return response(req.url.endsWith('/balance')?{code:0,data:{balance:'12.5',currency:'CNY'}}:{code:0,data:null});
  }});
  const result=app.probe();assert.equal(result.source,'web');assert.equal(metric(result,'Balance').value,'12.50 CNY');
 }
});

function openai(options) {
 return load('openai-api',{env:{OPENAI_API_KEY:'fixture-key',...options.env},provider:{settings:options.settings||{}},request:req=>{
  if(req.url.includes('/v1/organization/')) return response({},403);
  assert.ok(req.url.endsWith('/v1/dashboard/billing/credit_grants'));
  return response({total_granted:100,total_used:25,total_available:75,grants:{data:[{expires_at:1800000000},{expires_at:1}]}});
 }});
}

test('OpenAI sends OpenAI-Organization only on the legacy credit grants request',()=>{
 for(const [options,expected] of [
  [{settings:{OPENAI_ORGANIZATION_ID:' org-config '},env:{OPENAI_ORGANIZATION_ID:'org-env'}},'org-config'],
  [{env:{OPENAI_ORGANIZATION_ID:'org-env',OPENAI_ORG_ID:'org-alias'}},'org-env'],
  [{settings:{OPENAI_ORGANIZATION_ID:' '},env:{OPENAI_ORG_ID:'org-alias'}},'org-alias'],
 ]) {
  const app=openai(options);const result=app.probe();
  assert.equal(metric(result,'Available').value,'$75.00');
  assert.ok(app.requests.some(req=>req.url.includes('/v1/organization/costs')));
  for(const req of app.requests) {
   const grants=req.url.endsWith('/credit_grants');
   assert.equal(req.headers['OpenAI-Organization'],grants?expected:undefined);
  }
 }
});

test('OpenAI omits the organization header when none is configured',()=>{
 const app=openai({});app.probe();
 assert.ok(app.requests.every(req=>!Object.prototype.hasOwnProperty.call(req.headers,'OpenAI-Organization')));
});

test('new MiMo and OpenAI environment names are exposed by the native host',()=>{
 const host=fs.readFileSync(path.join(ROOT,'crates/usagestat-plugins/src/host_api.rs'),'utf8').split('const ENV_ALLOWLIST')[1].split('];',1)[0];
 const manifest=JSON.parse(fs.readFileSync(path.join(ROOT,'plugins/mimo/plugin.json'),'utf8'));
 assert.deepEqual(manifest.supportedModes,['api','web']);
 for(const name of [...manifest.envVars,'OPENAI_ORGANIZATION_ID','OPENAI_ORG_ID']) assert.ok(host.includes(`"${name}"`),name);
});
