const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {load,metric,response} = require('./provider-sync-harness.cjs');

// Synthetic provider responses, with the provider's units and account scopes.
const apis = require('./fixtures/bundled-api-readback.json');
function api(id,fixture,extra={}) {
  return load(id,{provider:{apiKey:'fixture-key',workspaceId:fixture.scope,settings:fixture.settings,...extra.provider},
    env:{MESSAGES:'invalid-json'},request:req=>{
      if(id==='futureagi') assert.equal(req.headers['X-Secret-Key'],'fixture-secondary');
      if(id==='cosmic') assert.equal(new URL(req.url).searchParams.get('project_id'),'project-fixture');
      if(id==='exa') assert.ok(new URL(req.url).pathname.includes('/key-fixture/usage'));
      if(id==='flora') assert.ok(new URL(req.url).pathname.includes('/ws_fixture/usage'));
      if(id==='munito' && req.url.endsWith('/organization')) return response('private-optional-body',503);
      return response(id==='soniox' && req.url.endsWith('/usage/monthly') ? fixture.monthly : fixture.body);
    },...extra});
}
for(const [id,fixture] of Object.entries(apis)) {
  test(`${id} reads its actual billing scope without inventing a daily charge`,async()=>{
    const app=api(id,fixture),out=await app.probe();
    assert.equal(out.source,'api');
    assert.equal(metric(out,fixture.label)?.[fixture.used===undefined?'value':'used'],fixture.used??fixture.value);
    assert.deepEqual(app.ingested,[]);
    if(fixture.quotaLabel) assert.equal(metric(out,fixture.quotaLabel).used,25);
    else if(fixture.used===undefined) assert.ok(out.lines.every(line=>line.type!=='progress'));
    if(id==='futureagi') assert.equal(metric(out,'AI credits').limit,100);
    if(id==='flora') assert.equal(metric(out,'Spent this month').value,'Unavailable');
  });
  test(`${id} rejects failed or malformed readback and never borrows another account's credentials`,async()=>{
    for(const status of [401,403,429,503]) {
      const app=api(id,fixture,{request:()=>response('private-payload',status)});
      await assert.rejects(app.probe,error=>!error.message.includes('private-payload')&&!error.message.includes('fixture-key'));
      assert.equal(app.requests.length,1);
    }
    await assert.rejects(api(id,fixture,{request:()=>response({})}).probe);
    const manifest=JSON.parse(fs.readFileSync(path.join(__dirname,'../plugins',id,'plugin.json'),'utf8'));
    const ambient=Object.fromEntries(manifest.envVars.map(key=>[key,key==='FLORA_WORKSPACE_ID' ? 'ws_fixture' : 'ambient-private']));
    const scoped=load(id,{provider:{instanceId:id+'-other'},env:ambient,request:()=>assert.fail('Borrowed credentials')});
    await assert.rejects(scoped.probe);
    assert.equal(scoped.requests.length,0);
  });
}

test('TypeSafe sends the browser User-Agent for billing, discovery and action requests',async()=>{
  const action='b'.repeat(40);
  const app=load('typesafe',{provider:{cookieHeader:'session=fixture'},request:req=>{
    assert.match(req.headers['User-Agent'],/^Mozilla\/5\.0 /);
    if(req.method==='POST') return response('1:{"ok":true,"data":{"billing":{"plan":"free","spent":0,"balance":5,"purchased":0,"cycleLabel":"September"}}}');
    return response(req.url.includes('/_next/') ? `"${action}",x,y,"getBillingOverviewResult"` : '<script src="/_next/static/chunks/app.js"></script>');
  }});
  assert.equal(metric(await app.probe(),'Balance').value,'USD 5.00');
  assert.equal(app.requests.length,3);
});
test('Hyper dashboard sessions report web source while bearer reads report API',async()=>{
  const app=load('hyper',{provider:{cookieHeader:'session=fixture',apiKey:'fixture-key'},request:req=>{
    assert.equal(req.url,'https://hyper.charm.land/');assert.equal(req.headers.Authorization,undefined);
    return response('<a href="/teams/12345678-1234-1234-1234-123456789abc">Team</a>Hypercredits Available<span class="text-5xl font-bold">12.5</span>');
  }});
  const out=await app.probe();assert.equal(out.source,'web');assert.equal(metric(out,'Balance').value,'12.5 HC');
  const fallback=load('hyper',{provider:{cookieHeader:'session=expired',apiKey:'fixture-key'},request:req=>
    req.headers.Cookie ? response('login',200) : response({balance:0})});
  assert.equal((await fallback.probe()).source,'api');
  assert.equal(fallback.requests.length,2);
});

const web = {
  parallel:{label:'Balance',value:'USD 12.50',reply:req=>{
    assert.equal(req.method,'POST');assert.equal(req.bodyText,'{}');
    return {kind:'ok',data:{balance:1250}};
  }},
  tinyapi:{label:'Available credits',value:'12.5 credits',reply:()=>({success:true,data:{totalAvailable:12.5}})},
  traecn:{label:'Credits',used:25,reply:req=>{
    if(req.url.endsWith('GetUserToken')) return {Result:{Token:'ephemeral-fixture'}};
    assert.equal(req.headers.Authorization,'Cloud-IDE-JWT ephemeral-fixture');
    return {is_credits_billing:true,usage_summary:{total_amount:100,consumed_amount:25},user_entitlement_pack_list:[]};
  }},
  langdock:{opaque:true,label:'Session',used:125,reply:()=>[{result:{data:{json:{hasIncludedUsageLimits:true,
    planUsage:{sessionUsageLimitsEnabled:true,sessionUsagePercent:125,weeklyUsagePercent:25}}}}}]},
  claudeplatform:{opaque:true,label:'Balance',value:'USD 12.50',reply:req=>{
    if(req.url.endsWith('/organizations')) return [{uuid:'org-fixture',name:'Fixture org',capabilities:['api']}];
    if(req.url.endsWith('/credits')) return {amount:1250,currency:'USD'};
    return response('private-optional-spend',403);
  }},
  xapi:{opaque:true,label:'Balance',value:'USD 15.00',reply:req=>req.url.endsWith('/api/me')
    ? {account:{id:'account-fixture'}} : {credits:{balance:12.5},freeCredits:{balance:2.5}}},
};
for(const [id,fixture] of Object.entries(web)) {
  function app(override) {
    const handle=req=>{
      if(fixture.opaque) {assert.equal(req.cookieSession,'configured');assert.equal(req.headers.Cookie,undefined);}
      else assert.equal(req.headers.Cookie,'session=fixture');
      const result=override ? override(req) : fixture.reply(req);
      return result?.bodyText!==undefined ? result : response(result);
    };
    return load(id,{provider:{cookieHeader:'session=fixture'},request:handle,
      ...(fixture.opaque ? {cookieSessionAvailable:true,cookieSessionRequest:handle} : {})});
  }
  test(`${id} preserves manual session scope and returned quota or balance`,async()=>{
    const instance=app(),out=await instance.probe();
    assert.equal(out.source,'web');
    assert.equal(metric(out,fixture.label)?.[fixture.used===undefined?'value':'used'],fixture.used??fixture.value);
    assert.equal(instance.ingested.length,0);
    if(id==='claudeplatform') assert.equal(metric(out,'Spend').value,'Unavailable');
    if(id==='traecn') assert.ok([...instance.files.values()].every(value=>!value.includes('ephemeral-fixture')));
  });
  test(`${id} rejects failed sessions and malformed payloads without publishing zero usage`,async()=>{
    for(const status of [401,403,429,503]) await assert.rejects(app(()=>response('private-body',status)).probe,
      error=>!error.message.includes('private-body'));
    await assert.rejects(app(()=>response({})).probe);
    const other=load(id,{provider:{instanceId:id+'-other'},env:{[id.toUpperCase()+'_COOKIE']:'other-private'}});
    await assert.rejects(other.probe);assert.equal(other.requests.length,0);
    const off=load(id,{provider:{cookieHeader:'session=fixture',settings:{cookies:'off'}},cookieSessionAvailable:true,
      cookieSessionRequest:()=>assert.fail('Disabled cookies used')});
    await assert.rejects(off.probe);assert.equal(off.requests.length,0);
  });
}

test('Claude reset grant inventory is requested at most hourly across restarts and remains account scoped',()=>{
  const credentials={accessToken:'fixture',expiresAt:1999999999999,scopes:['user:profile'],subscriptionType:'pro'};
  const quota={five_hour:{utilization:10},seven_day:{utilization:25}};
  const first=load('claude',{source:'oauth',credentials,request:req=>req.url.endsWith('/profile') ? response({},503) :
    response({...quota,cedar_ember:{eligible:true,grants:[{resets_left:2,ends_at:'2026-09-05T12:10:00Z'}]}})});
  assert.equal(metric(first.probe(),'Rate Limit Resets').value,'2 available');
  assert.equal(first.requests.filter(req=>req.url.includes('cedar_ember=1')).length,1);
  const second=load('claude',{source:'oauth',credentials,files:Object.fromEntries(first.files),now:'2026-09-05T12:06:00Z',request:req=>{
    assert.ok(!req.url.includes('cedar_ember=1'));return response(quota);
  }});
  assert.equal(metric(second.probe(),'Rate Limit Resets').value,'2 available');
  const third=load('claude',{source:'oauth',credentials,files:Object.fromEntries(second.files),now:'2026-09-05T12:12:00Z',request:()=>response(quota)});
  assert.equal(metric(third.probe(),'Rate Limit Resets').value,'0 available','Retained grants expire at read time');
  const later=load('claude',{source:'oauth',credentials,files:Object.fromEntries(third.files),now:'2026-09-05T13:01:00Z',request:req=>{
    assert.ok(req.url.includes('cedar_ember=1'));return response(quota);
  }});
  assert.equal(metric(later.probe(),'Session').used,10);
  const another=load('claude',{source:'oauth',credentials:{...credentials,accessToken:'another-account'},files:Object.fromEntries(second.files),
    now:'2026-09-05T12:06:00Z',request:req=>req.url.endsWith('/profile') ? response({},503) : response(quota)});
  assert.equal(metric(another.probe(),'Rate Limit Resets'),undefined);
});

test('Ollama API balance keeps monthly allowance, purchased credits and legacy windows distinct',()=>{
  const app=load('ollama',{source:'api',provider:{apiKey:'fixture-key'},request:req=>{
    assert.equal(req.method,'GET');assert.equal(req.url,'https://ollama.com/api/balance');
    assert.equal(req.headers.Authorization,'Bearer fixture-key');
    return response({plan:'pro',included:{allowance_usd:20,balance_usd:15.6,
      period:{from:'2026-09-01T00:00:00Z',until:'2026-10-01T00:00:00Z'}},purchased:{balance_usd:-1.25}});
  }});
  const out=app.probe(),month=metric(out,'Monthly credits');
  assert.ok(Math.abs(month.used-4.4)<1e-9);assert.equal(month.limit,20);assert.equal(month.format.kind,'dollars');
  assert.equal(month.periodDurationMs,30*86400000);assert.equal(month.resetsAt,'2026-10-01T00:00:00.000Z');
  assert.equal(metric(out,'Purchased credits').value,'$-1.25');assert.equal(app.requests.length,1);
  for(const included of [{allowance_usd:0,balance_usd:0},{session:{remaining_percent:75},weekly:{used_percent:0}}]){
    const result=load('ollama',{source:'api',provider:{apiKey:'fixture-key'},request:()=>response({included})}).probe();
    assert.equal(metric(result,'Monthly credits'),undefined);
    if(included.session) {assert.equal(metric(result,'Session').used,25);assert.equal(metric(result,'Weekly').used,0);}
  }
  for(const included of [{allowance_usd:false,balance_usd:0},{allowance_usd:20,balance_usd:10,period:{until:'2026-02-30T00:00:00Z'}}]){
    assert.throws(load('ollama',{source:'api',provider:{apiKey:'fixture-key'},request:()=>response({included})}).probe);
  }
});

test('OpenCode drops only zero-usage full-session reset placeholders using the server clock',()=>{
  for(const [percent,reset,expected] of [[0,'2026-09-05T18:00:00Z',undefined],[1,'2026-09-05T18:00:00Z','2026-09-05T18:00:00.000Z'],
    [0,'2026-09-05T17:30:00Z','2026-09-05T17:30:00.000Z']]){
    const app=load('opencode-go',{source:'api',provider:{apiKey:'fixture'},request:()=>({...response({usage:{rolling:{percent,resetsAt:reset},weekly:{percent:0,resetsAt:reset}}}),
      headers:{DaTe:'Sat, 05 Sep 2026 13:00:00 GMT'}})});
    const out=app.probe();assert.equal(metric(out,'Session').resetsAt,expected);
    assert.equal(metric(out,'Weekly').resetsAt,new Date(reset).toISOString());
  }
});

test('Vercel personal budgets require their own token and match only the authenticated user',async()=>{
  const provider={apiKey:'gateway-private',workspaceId:'team-fixture',settings:{personalBudget:true,VERCEL_ACCESS_TOKEN:'account-fixture'}};
  for(const scopeId of ['user-fixture','usr_user-fixture']) {
    const app=load('vercel',{provider,request:req=>{
      assert.equal(req.headers.Authorization,'Bearer account-fixture');
      assert.equal(new URL(req.url).host,'api.vercel.com');
      if(req.url.endsWith('/v2/user')) return response({user:{id:'user-fixture'}});
      assert.equal(new URL(req.url).searchParams.get('teamId'),'team-fixture');
      return response({budgets:[{scopeType:'user',scopeId:'other-user',active:true,archived:false,limitAmount:999,currentSpend:999},
        {scopeType:'user',scopeId,active:true,archived:false,limitAmount:100,currentSpend:25,refreshPeriod:'monthly'}]});
    }});
    const out=await app.probe();assert.equal(metric(out,'Personal budget').used,25);
    assert.equal(metric(out,'Personal budget').resetsAt,'2026-10-01T00:00:00.000Z');assert.equal(app.ingested.length,0);
  }
  const missing=load('vercel',{provider:{...provider,settings:{personalBudget:true}},request:()=>assert.fail('Gateway key was reused')});
  await assert.rejects(missing.probe,/VERCEL_ACCESS_TOKEN/);assert.equal(missing.requests.length,0);
  const repeated=load('vercel',{provider,request:req=>response(req.url.endsWith('/v2/user') ? {user:{id:'user-fixture'}} : {budgets:[],nextCursor:'repeat'})});
  await assert.rejects(repeated.probe,/unrecognized/);assert.equal(repeated.requests.length,3);
  assert.throws(load('vercel',{provider:{...provider,settings:{personalBudget:'false'}}}).probe,/boolean/);
});

test('Qwen Team is an explicit subscription scope and empty subscriptions remain no-data',async()=>{
  function qwen(empty=false) {
    return load('qwencloud',{provider:{cookieHeader:'session=fixture',settings:{teamTokenPlan:true}},cookieSessionAvailable:true,cookieSessionRequest:req=>{
      assert.equal(req.cookieSession,'configured');assert.equal(req.headers.Cookie,undefined);
      if(req.url.endsWith('/tool/user/info.json')) return response({data:{secToken:'ephemeral-csrf'}});
      const form=new URLSearchParams(req.bodyText);assert.equal(form.get('sec_token'),'ephemeral-csrf');
      if(form.get('action')==='LoadHumanInfo') return response({successResponse:true,data:{Data:{SellerInfoDto:{Nbid:'billing-fixture'}}}});
      assert.equal(form.get('action'),'GetSeatSubscriptionSummary');assert.equal(JSON.parse(form.get('params')).Nbid,'billing-fixture');
      return response({successResponse:true,data:{Data:empty ? null : {StartTime:Date.parse('2026-09-01T00:00:00Z'),EndTime:Date.parse('2026-10-01T00:00:00Z'),ProductCode:'sfm_tokenplanteams_dp_intl',
        SubscriptionGroupList:[{SpecType:'pro',SubscriptionAssignedNumber:1,SubscriptionTotalNumber:2,EquityList:[{EquityCode:'credit_value',TotalValue:'100',SurplusValue:'75'}]}]}}});
    }});
  }
  const app=qwen(),out=await app.probe();assert.equal(metric(out,'Team credits').used,25);assert.equal(out.plan,'Pro Team');
  assert.equal(app.requests.length,3);assert.equal(app.ingested.length,0);
  assert.equal((await qwen(true).probe()).state,'no-data');
  const named=load('qwencloud',{provider:{instanceId:'other',settings:{teamTokenPlan:true}},env:{QWEN_CLOUD_COOKIE:'ambient'}});
  await assert.rejects(named.probe);assert.equal(named.requests.length,0);
  assert.throws(load('qwencloud',{provider:{settings:{teamTokenPlan:'false'}}}).probe,/boolean/);
});

test('Antigravity merges known shared pools and keeps Argon separate from future buckets',()=>{
  const app=load('antigravity');
  const first={groups:[{displayName:'Gemini 4 Argon',buckets:[{bucketId:'argon-weekly',window:'weekly',remainingFraction:0.25}]},
    {displayName:'Gemini Models',buckets:[{bucketId:'gemini-5h',remainingFraction:null},{bucketId:'gemini-image-5h',remainingFraction:0}]}]};
  const second={groups:[{displayName:'Gemini Models',buckets:[{bucketId:'gemini-5h',remainingFraction:0.75},{bucketId:'gemini-weekly',remainingFraction:0.5}]},
    {displayName:'Claude/GPT',buckets:[{bucketId:'3p-5h',remainingFraction:1}]}]};
  const lines=app.ctx.util.groupedQuotaLines([first,second]);
  assert.deepEqual(Array.from(lines,line=>[line.label,line.used]),[['Gemini Session',25],['Gemini Weekly',50],['Gemini 4 Argon Weekly',75],['Claude/GPT Session',0]]);
  assert.equal(app.ctx.util.groupedQuotaLines({}),null);
  assert.deepEqual(Array.from(app.ctx.util.groupedQuotaLines({groups:[]})),[]);
  const unknown=app.ctx.util.groupedQuotaLines({groups:[{displayName:'Gemini Models',buckets:[{bucketId:'future-weekly',remainingFraction:0.5}]}]});
  assert.deepEqual(Array.from(unknown),[]);
});

test('Claude cloud-session credits stay separate from subscription quotas and extra usage',()=>{
  const credentials={accessToken:'fixture',expiresAt:1999999999999,scopes:['user:profile'],subscriptionType:'pro'};
  function run(block) {return load('claude',{source:'oauth',credentials,request:req=>req.url.endsWith('/profile') ? response({},503) :
    response({five_hour:{utilization:10},iguana_necktie:block})}).probe();}
  const out=run({limit_dollars:100,used_dollars:10,remaining_dollars:75,resets_at:'2026-10-01T00:00:00Z'});
  assert.equal(metric(out,'Cloud credits').value,'$75.00 remaining');
  assert.match(metric(out,'Cloud credits').subtitle,/Expires/);assert.equal(metric(out,'Session').used,10);
  assert.equal(metric(run({limit_dollars:100,remaining_dollars:0}),'Cloud credits').value,'$0.00 remaining');
  assert.equal(metric(run({limit_dollars:100,remaining_dollars:75,locked_reason:'private-reason'}),'Cloud credits').value,'Unavailable');
  assert.equal(metric(run({limit_dollars:100,remaining_dollars:75,locked_reason:' '}),'Cloud credits').value,'$75.00 remaining');
  assert.equal(metric(run({limit_dollars:100,remaining_dollars:75,resets_at:'2026-08-01T00:00:00Z'}),'Cloud credits').value,'Expired');
  assert.equal(metric(run({limit_dollars:100,remaining_dollars:75,locked_reason:'private-reason',resets_at:'2026-08-01T00:00:00Z'}),'Cloud credits').value,'Expired');
  for(const block of [null,{},[],{limit_dollars:true,remaining_dollars:0},{limit_dollars:100,remaining_dollars:101},{limit_dollars:100,remaining_dollars:75,resets_at:'invalid'}]) {
    const result=run(block);assert.equal(metric(result,'Cloud credits'),undefined);assert.equal(metric(result,'Session').used,10);
  }
});

for(const id of ['cursor','cursor-nightly']) test(`${id} makes the active unified weekly pool primary without duplicating model pools`,()=>{
  function run(extra={}) {return load(id,{credentials:{accessToken:'fixture'},request:req=>{
    if(req.url.endsWith('GetCurrentPeriodUsage')) return response({enabled:true,planUsage:{totalPercentUsed:10,autoPercentUsed:5,apiPercentUsed:15}});
    if(req.url.endsWith('GetPlanInfo')) return response({planInfo:{planName:'unified_ultra'}});
    if(req.url.endsWith('GetSandUsageStatus')) return response({isUnified:true,hasNonZeroIncludedLimit:true,usagePercent:25,
      currentPeriodStart:'2026-09-03T00:00:00Z',nextResetTimestampUtc:'2026-09-10T00:00:00Z',...extra});
    return response({});
  }}).probe();}
  const result=run();assert.equal(metric(result,'Weekly').used,25);
  assert.equal(metric(result,'Weekly').periodDurationMs,7*86400000);
  for(const label of ['Total usage','Other Models','Cursor Models','Grok Bot usage']) assert.equal(metric(result,label),undefined);
  assert.equal(metric(run({isUnified:false}),'Total usage').used,10);
  assert.equal(metric(run({currentPeriodStart:'2026-09-05T00:00:00Z'}),'Weekly').used,25,'A paid weekly pool can start mid-week');
  assert.equal(metric(run({nextResetTimestampUtc:'2026-09-04T00:00:00Z',currentPeriodStart:'2026-08-28T00:00:00Z'}),'Weekly'),undefined);
});

test('xAI invoice preview is an optional live estimate and is never stored as billed spend',()=>{
  function run(preview,status=200) {return load('xai',{provider:{apiKey:'fixture',workspaceId:'team'},request:req=>{
    if(req.url.endsWith('/prepaid/balance')) return response({total:{val:'-10000'}});
    if(req.url.endsWith('/postpaid/invoice/preview')) return response(preview,status);
    return response({timeSeries:[]});
  }});}
  const app=run({coreInvoice:{amountAfterVat:'2500'}}),out=app.probe();
  assert.equal(metric(out,'Live remaining (est.)').value,'$75.00');
  assert.equal(metric(out,'Balance').value,'$100.00');assert.equal(app.ingested.length,0);
  for(const [data,status] of [[{},503],[{coreInvoice:{amountAfterVat:'invalid'}},200],[{coreInvoice:{amountAfterVat:false}},200]]){
    const result=run(data,status).probe();assert.equal(metric(result,'Balance').value,'$100.00');
    assert.equal(metric(result,'Live remaining (est.)'),undefined);
  }
});

for (const id of ['cursor','cursor-nightly']) test(`${id} retains the enterprise request allowance as primary`,()=>{
  const app=load(id,{credentials:{accessToken:'e30.'+Buffer.from(JSON.stringify({sub:'user-fixture'})).toString('base64url')+'.signature'},request:req=>{
    if(req.url.endsWith('GetCurrentPeriodUsage')) return response({enabled:true});
    if(req.url.endsWith('GetPlanInfo')) return response({planInfo:{planName:'enterprise'}});
    if(req.url.includes('/api/usage')) return response({'gpt-4':{numRequests:450,maxRequestUsage:500}});
    if(req.url.endsWith('GetSandUsageStatus')) return response({isUnified:true,hasNonZeroIncludedLimit:true,usagePercent:25,nextResetTimestampUtc:'2026-09-10T00:00:00Z'});
    return response({});
  }});
  const out=app.probe();assert.equal(out.lines[0].label,'Requests');
  assert.equal(metric(out,'Requests').used,450);assert.equal(metric(out,'Requests').limit,500);
  assert.equal(metric(out,'Weekly'),undefined);assert.equal(metric(out,'Grok Bot usage').used,25);
});

test('Kimi accepts ratio pools, preserves conflicting exhausted counters, and separates expiring gifts',()=>{
  function run(data,stats={}) {return load('kimi',{provider:{apiKey:'key',cookieHeader:'kimi-auth=web'},request:req=>
    response(req.method==='GET' ? data : stats)}).probe();}
  const reset='2026-09-10T00:00:00Z';
  const data={usages:{limit_5h:{used_ratio:0.1,reset_time:reset},limit_7d:{used_ratio:0.4,reset_time:reset},limit_month_total:{used_ratio:0.7,reset_time:'2026-10-01T00:00:00Z'}}};
  const stats={giftBalances:[{type:'GIFT',feature:'FEATURE_OMNI',displayName:'Welcome bonus',amountUsedRatio:0.25,expireTime:'2026-10-01T00:00:00Z'},
    {type:'GIFT',amountUsedRatio:0.5,expireTime:'2026-09-01T00:00:00Z'}, {type:'SUBSCRIPTION',amountUsedRatio:0.2}, {type:'GIFT',amountUsedRatio:false}]};
  const out=run(data,stats);assert.equal(metric(out,'Session').used,10);assert.equal(metric(out,'Weekly').used,40);assert.equal(metric(out,'Monthly').used,70);
  assert.equal(metric(out,'Welcome bonus').value,'75% remaining');assert.match(metric(out,'Welcome bonus').subtitle,/Expires/);
  assert.equal(out.lines.filter(line=>line.type==='text').length,1);
  const exhausted=run({...data,usages:{...data.usages,limit_month_total:{used_ratio:1}}});
  assert.equal(exhausted.lines[0].label,'Monthly');assert.equal(exhausted.lines[0].used,100);
  const membershipExhausted=run(data,{subscriptionBalance:{amountUsedRatio:1,feature:'FEATURE_OMNI',type:'SUBSCRIPTION'}});
  assert.equal(membershipExhausted.lines[0].label,'Monthly');assert.equal(metric(membershipExhausted,'Monthly').used,100);
  assert.equal(membershipExhausted.lines.filter(line=>line.label==='Monthly').length,1);
  assert.equal(metric(run({...data,usage:{limit:100,used:90,resetTime:reset}}),'Weekly').used,90);
  assert.equal(metric(run({...data,usage:{limit:100,used:90,resetTime:'2026-10-10T00:00:00Z'}}),'Weekly').used,40);
  const lapsed=load('kimi',{source:'web',provider:{cookieHeader:'kimi-auth=web'},request:req=>response(req.url.endsWith('GetUsages') ? {usages:[]} : {subscriptionBalance:{amountUsedRatio:1,feature:'FEATURE_OMNI',type:'SUBSCRIPTION'}})}).probe();
  assert.equal(metric(lapsed,'Monthly').used,100);assert.equal(metric(lapsed,'Weekly'),undefined);
});
