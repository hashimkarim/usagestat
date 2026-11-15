const {test}=require('node:test');
const assert=require('node:assert/strict');
const {load,metric,response}=require('./provider-sync-harness.cjs');

test('new API providers preserve balance and reserved credits without inventing spend',async()=>{
  const silicon=load('siliconflow',{provider:{apiKey:'fixture'},request:req=>{
    assert.equal(req.url,'https://api.siliconflow.com/v1/user/info');
    assert.equal(req.headers.Authorization,'Bearer fixture');
    return response({code:20000,status:true,data:{totalBalance:'12.5',balance:'10',chargeBalance:'2.5'}});
  }});
  const out=await silicon.probe();assert.equal(metric(out,'Balance').value,'USD 12.50');assert.equal(silicon.ingested.length,0);
  const tripo=load('tripo',{provider:{apiKey:'fixture'},request:()=>response({code:0,data:{balance:20,frozen:3}})});
  const result=await tripo.probe();assert.equal(metric(result,'Balance').value,'20');assert.equal(metric(result,'Reserved').value,'3');
  assert.equal(tripo.ingested.length,0);
});

test('OpenDesign uses the selected opaque session and validates workspace identity',async()=>{
  for(const match of [true,false]) {
    const app=load('opendesign',{provider:{cookieHeader:'session=fixture'},cookieSessionAvailable:true,cookieSessionRequest:req=>{
      assert.equal(req.cookieSession,'configured');assert.equal(req.headers.Cookie,undefined);
      return response(req.url.endsWith('/current') ? {workspaceId:'selected',workspaceName:'Fixture'} :
        {workspaceId:match?'selected':'foreign',eligible:true,windows:[{durationSeconds:18000,usedCredits:25,limitCredits:100}]});
    }});
    if(match) assert.equal(metric(await app.probe(),'Session').used,25);
    else await assert.rejects(app.probe,/format/);
  }
});

test('Perplexity manual sessions report exact counts and separate balance from spend',()=>{
  const app=load('perplexity',{provider:{cookieHeader:'__Secure-next-auth.session-token=fixture'},cookieSessionAvailable:true,cookieSessionRequest:req=>{
    assert.equal(req.headers.Cookie,undefined);assert.equal(req.cookieSession,'configured');
    const path=new URL(req.url).pathname;
    return response(path==='/api/auth/session'?{user:{subscription_tier:'pro'}}:
      path==='/rest/rate-limit/status'?{modes:{pro_search:{remaining_detail:{kind:'exact',remaining:0}}}}:{balance_cents:1250});
  }});
  const result=app.probe();assert.equal(result.plan,'Pro');assert.equal(metric(result,'Queries').value,'0 remaining');
  assert.equal(metric(result,'API credits').value,'$12.50');assert.equal(app.ingested.length,0);
});

test('Synthetic new weekly plans preserve sub-percent remaining values and retire legacy counts',()=>{
 const app=load('synthetic',{env:{SYNTHETIC_API_KEY:'fixture'},request:()=>response({planWeeklyLimit:{percentRemaining:.5},subscription:{requests:100,limit:100}})});
 const result=app.probe();assert.equal(metric(result,'Weekly plan limit').used,99.5);
 assert.equal(metric(result,'Subscription'),undefined);
});

test('Kimi monthly detail uses Code shares from the same monthly source',()=>{
 const app=load('kimi',{source:'api',provider:{apiKey:'fixture'},request:()=>response({usages:{limit_month_total:{used_ratio:.7},limit_month_code:{used_ratio:.2}}})});
 const result=app.probe();assert.equal(metric(result,'Monthly Kimi Code').value,'20% used');
 assert.equal(metric(result,'Monthly other features').value,'50% used');
});

test('Helmcode credit plan is independent of legacy token quota and fails closed on unavailable credit API',async()=>{
 const app=load('helmcode',{provider:{cookieHeader:'session=fixture'},request:req=>{
   assert.ok(req.url.endsWith('/api/usage/credits'));
   return response({enabled:true,allowanceCredits:100,usedCredits:25,models:[{model:'Fixture',creditsThisMonth:10}]});
 }});
 assert.equal(metric(await app.probe(),'Credits').used,25);assert.equal(app.requests.length,1);
 await assert.rejects(load('helmcode',{provider:{cookieHeader:'session=fixture'},request:()=>response({},503)}).probe,/unavailable/);
});

test('Google Developer reads a scoped account credit without treating its remaining balance as spend',async()=>{
 const offer=[];offer[6]=[['USD',10],['USD',7],'billingAccounts/fixture'];
 const benefit=[];benefit[0]=19;benefit[9]=offer;
 const app=load('googledeveloper',{provider:{cookieHeader:'session=fixture'},cookieSessionAvailable:true,cookieSessionRequest:req=>{
   assert.equal(req.cookieSession,'configured');assert.equal(req.headers.Cookie,undefined);
   if(req.method==='GET') return response('<script>var WIZ_global_data={"SNlM0e":"csrf-fixture","oPEP7c":"fixture@example.test"};</script>');
   return response(JSON.stringify([['wrb.fr','Ro0AYc',JSON.stringify([[benefit]])]]));
 }});
 const result=await app.probe();assert.equal(metric(result,'Credits').used,30);
 assert.equal(metric(result,'Balance').value,'USD 7.00');assert.equal(metric(result,'Monthly credits'),undefined);
 assert.equal(app.ingested.length,0);
});

test('Cursor ciphertext never becomes an Authorization header or triggers ambient fallback for explicit credentials',()=>{
 const app=load('cursor',{credentials:{accessToken:'cursor-safe-storage:v1:ciphertext'},request:()=>assert.fail('Ciphertext was sent')});
 assert.throws(app.probe,error=>error.code==='credential-malformed');assert.equal(app.requests.length,0);
});

const {providerHarness}=require('./provider-harness.cjs');
test('Codex Free uses the reported fixed 30-day window rather than a five-hour session',()=>{
 const app=providerHarness('codex',{host:{codex:{readAuth:()=>JSON.stringify({source:'native',storage:'file',profileKey:'fixture',revision:'fixture',auth:{tokens:{access_token:'fixture'},last_refresh:new Date().toISOString()}})}},
 http:()=>response({plan_type:'free',rate_limit:{primary_window:{used_percent:25,limit_window_seconds:2592000}}})});
 const result=app.probe();assert.equal(result.lines.find(l=>l.label==='Monthly').periodDurationMs,30*86400000);
 assert.equal(result.lines.find(l=>l.label==='Monthly').used,25);
});


test('JetBrains explicit IDE selection never consumes another IDE log even with live observations enabled',()=>{
 const xml='<application><component name="AIAssistantQuotaManager2"><option name="quotaInfo" value="{&quot;current&quot;:10,&quot;maximum&quot;:100}" /></component></application>';
 const app=providerHarness('jetbrains-ai-assistant',{settings:{configDir:'/selected',liveQuota:true},files:{'/selected/options/AIAssistantQuotaManager2.xml':xml}});
 const quota=app.probe();assert.equal(quota.lines[0].used,10);
 assert.ok(app.calls.files.every(path=>!path.includes('.junie') && !path.includes('.cache')));
 assert.equal(app.calls.http.length,0);
});

test('unconfigured local providers report missing data without a ProviderConfig object',()=>{
 for(const id of ['jetbrains-ai-assistant','perplexity']) {
  const app=providerHarness(id);delete app.ctx.provider;
  assert.throws(app.probe,error=>!String(error).includes('TypeError'));
 }
});


test('Cursor stable CLI fallback is read-only and unavailable to Nightly or named accounts',()=>{
 const token='e30.'+Buffer.from(JSON.stringify({sub:'fixture',exp:4102444800})).toString('base64url')+'.sig';
 for(const [id,instanceId,allowed] of [['cursor','cursor',true],['cursor-nightly','cursor-nightly',false],['cursor','other-cursor',false]]) {
  let reads=0;
  const app=load(id,{provider:{instanceId},sqlite:()=>JSON.stringify([{value:'cursor-safe-storage:v1:ciphertext'}]),request:req=>{
   assert.equal(req.headers.Authorization,'Bearer '+token);
   return response(req.url.endsWith('GetCurrentPeriodUsage')?{enabled:true,planUsage:{totalPercentUsed:10}}:{});
  }});
  app.ctx.host.cursorPaths={resolveStateDb:()=>'/fixture.db',sharedCredentialsAllowed:true,readCliAuth:()=>{reads++;return JSON.stringify({accessToken:token});}};
  if(allowed) assert.equal(metric(app.probe(),'Total usage').used,10);
  else assert.throws(app.probe,/Not logged in/);
  assert.equal(reads,allowed?1:0);
 }
});

test('JetBrains optional-source read errors retain valid XML observations',()=>{
 const xml='<option name="quotaInfo" value="{&quot;maximum&quot;:100,&quot;current&quot;:25}"/>';
 const app=providerHarness('jetbrains-ai-assistant',{settings:{liveQuota:true}});
 const root=app.ctx.host.fs.appSupportPath('JetBrains');
 app.files.set(app.normalize(root+'/IntelliJIdea2026.2/options/AIAssistantQuotaManager2.xml'),xml);
 app.files.set(app.normalize(app.ctx.host.fs.cacheDir+'/JetBrains/IntelliJIdea2026.2/log/idea.log'),'x'.repeat(2097153));
 delete app.ctx.host.log.debug;
 assert.equal(app.probe().lines[0].used,25);
});

test('JetBrains bounded session scans consider the newest timestamp-named sessions',()=>{
 const app=providerHarness('jetbrains-ai-assistant',{settings:{liveQuota:true}});
 for(let i=1;i<=65;i++) {
  const stamp=new Date(Date.UTC(2026,8,1,0,i)).toISOString().replace(/:/g,'-');
  app.files.set(app.normalize('~/.junie/sessions/'+stamp+'/events.jsonl'),JSON.stringify({timestamp:new Date(Date.UTC(2026,8,1,0,i)).toISOString(),quota:{maximum:100,current:i===65?90:10}}));
 }
 assert.equal(app.probe().lines[0].used,90);
});

for(const platform of ['linux','windows']) test('JetBrains logs use native redirected roots on '+platform,()=>{
 const app=providerHarness('jetbrains-ai-assistant',{platform,settings:{liveQuota:true},cacheDir:'/redirected-cache'});
 const root=platform==='windows'?app.ctx.host.fs.localAppDataPath('JetBrains'):app.ctx.host.fs.cacheDir+'/JetBrains';
 app.files.set(app.normalize(root+'/IntelliJIdea2026.2/log/idea.log'),'2026-09-01 00:00:00,000 QuotaManager2Impl New quota info '+JSON.stringify({maximum:100,current:75}));
 assert.equal(app.probe().lines[0].used,75);
});
