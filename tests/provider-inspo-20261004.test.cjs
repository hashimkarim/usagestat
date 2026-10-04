const {test}=require('node:test');
const assert=require('node:assert/strict');
const {spawnSync}=require('node:child_process');
const fs=require('node:fs');
const path=require('node:path');
const os=require('node:os');
const {load,metric,response,ROOT}=require('./provider-sync-harness.cjs');
const platform=process.platform==='win32'?'windows':process.platform==='darwin'?'macos':'linux';

function lithos(balance=12345000000,status=200,spendStatus=200){
  return load('lithosai',{cookieSessionAvailable:true,cookieSessionRequest:req=>{
    assert.equal(req.cookieSession,'configured');
    assert.equal(req.headers.Cookie,undefined,'Opaque session does not expose a cookie to the parser');
    assert.equal(req.headers['X-Console-Csrf'],undefined,'Native host supplies the scoped echo');
    assert.equal(req.method,'GET');
    if(req.url.endsWith('/api/me'))return response({activeOrganization:{id:'org_one',name:'Fixture org'},user:{email:'fixture@example.test'}},status);
    assert.equal(req.headers['X-Organization-Id'],'org_one');
    if(req.url.includes('/spend?'))return response({start:'2026-09-01',end:'2026-09-05',days:[{day:'2026-09-04',nanos:2000000000},{day:'2026-09-05',nanos:1000000000}]},spendStatus);
    return response({balanceNanos:balance,hasCard:false,onHold:false});
  }});
}
test('LithosAI preserves signed prepaid balances without a fabricated quota or spend',async()=>{
  for(const [balance,expected] of [[12345000000,'USD 12.35'],[-1000000000,'USD -1.00'],[0,'USD 0.00'],[1000,'Less than $0.01']]){
    const result=await lithos(balance).probe();
    assert.equal(metric(result,'Balance').value,expected);
    assert.ok(result.lines.every(line=>line.type!=='progress'));
    assert.equal(metric(result,'Prepaid credits'),undefined,'Balance is not reported as usage.cost');
    assert.equal(result.source,'web');
    assert.equal(metric(result,'Today (UTC)').value,'USD 1.00');
    assert.equal(metric(result,'This month (UTC)').value,'USD 3.00');
  }
});
test('LithosAI optional spend denial retains balance; primary denial never returns a zero',async()=>{
  const result=await lithos(5000000000,200,503).probe();
  assert.equal(metric(result,'Balance').value,'USD 5.00');
  assert.equal(metric(result,'Spend').value,'Unavailable');
  for(const status of [401,403,429,503])await assert.rejects(lithos(0,status).probe);
  await assert.rejects(lithos(1.5).probe,/integer amount/);
});

function workbuddy(packages,listingStatus=200){
  return load('workbuddy',{cookieSessionAvailable:true,cookieSessionRequest:req=>{
    assert.equal(req.cookieSession,'configured');assert.equal(req.headers.Cookie,undefined);
    assert.equal(req.method,'POST');assert.ok(req.url.startsWith('https://www.workbuddy.cn/billing/meter/'));
    if(req.url.endsWith('summary'))return response({code:0,data:{SubscriptionPackageName:'Fixture Pro',Packages:packages}});
    const body=JSON.parse(req.bodyText);assert.equal(body.PageSize,100);assert.ok(body.PackageCodes.length>0);
    return response({code:0,data:{Accounts:[{CapacityUnit:'credits',CycleEndTime:'2026-09-30 23:59:59'}]}},listingStatus);
  }});
}
test('WorkBuddy reconciles reported remaining credits and China cycle reset',async()=>{
  const result=await workbuddy([{CapacityUnit:'credits',CycleTotalCapacity:'500',CycleRemainCapacity:'450',CycleFrozenCapacity:'5'},
    {CapacityUnit:'tokens',CycleTotalCapacity:'999',CycleRemainCapacity:'1'}]).probe();
  const credits=metric(result,'Credits');assert.equal(credits.used,10);assert.equal(credits.detail,'450 / 500 credits left');
  assert.equal(credits.resetsAt,'2026-09-30T16:00:00.000Z');assert.equal(metric(result,'Reserved').value,'5');
  assert.equal(result.plan,'Fixture Pro');
});
test('WorkBuddy optional listing failure keeps balance, and zero total is text',async()=>{
  const result=await workbuddy([{CapacityUnit:'credits',CycleTotalCapacity:100,CycleRemainCapacity:75}],401).probe();
  assert.equal(metric(result,'Credits').used,25);assert.equal(metric(result,'Credits').resetsAt,undefined);
  const zero=await workbuddy([{CapacityUnit:'credits',CycleTotalCapacity:0,CycleRemainCapacity:0}]).probe();
  assert.equal(metric(zero,'Left').value,'0');assert.equal(metric(zero,'Total').value,'0');
  assert.ok(zero.lines.every(line=>line.type!=='progress'));
  await assert.rejects(workbuddy([{CapacityUnit:'credits',CycleTotalCapacity:'bogus',CycleRemainCapacity:1}]).probe);
});
test('new manual providers remain paused without cookies and named instances cannot inherit cookies',async()=>{
  for(const id of ['museai','lithosai','workbuddy']){
    const app=load(id,{provider:{instanceId:id+'-other'},env:{[id.toUpperCase()+'_COOKIE']:'session=ambient'}});
    await assert.rejects(app.probe);assert.equal(app.requests.length,0);
  }
});

const action='a'.repeat(40), actionPath='/test/data/museai/bundled-actions.json';
const flight=()=>response('1:'+JSON.stringify({success:true,subscription:{usage:{percentUsed:25,resetsAt:1790800000},tier:{name:'Fixture Pro'},
  topupBalance:1500000,topupTotal:2000000,agreement:{currentPeriodEndTime:1790900000}}}));
test('muse.ai recovers a stale action, discovers beyond 40 chunks and persists only the public ID',async()=>{
  const app=load('museai',{provider:{cookieHeader:'session=fixture-private'},files:{[actionPath]:JSON.stringify({actionID:'b'.repeat(40)})},request:req=>{
    if(req.method==='POST')return req.headers['Next-Action']===action?flight():response('Server action not found',404);
    if(req.url.endsWith('/'))return response(Array.from({length:48},(_,i)=>`<script src="/_next/static/chunks/chunk${i}.js"></script>`).join(''));
    assert.equal(req.headers.Cookie,undefined,'Static code discovery sends no session cookies');
    if(req.url.endsWith('chunk47.js'))return response('.A(123).then(({SettingsX})=>{});[123,e=>{e.v(e=>Promise.all(["static/chunks/settings.js"]))');
    if(req.url.endsWith('settings.js'))return response(`"${action}",x,y,"fetchSubscriptionAction"`);
    return response('');
  }});
  const result=await app.probe();assert.equal(metric(result,'Weekly').used,25);assert.equal(metric(result,'Weekly').periodDurationMs,604800000);
  assert.equal(metric(result,'Additional tokens').value,'$1.50 left');assert.ok(app.requests.length>40&&app.requests.length<=160);
  assert.equal(app.files.get(actionPath),JSON.stringify({actionID:action}));
  assert.ok(!app.files.get(actionPath).includes('fixture-private'));
  const cached=load('museai',{provider:{cookieHeader:'session=fixture-private'},files:Object.fromEntries(app.files),request:req=>{
    assert.equal(req.method,'POST');assert.equal(req.headers['Next-Action'],action);return flight();
  }});assert.equal(metric(await cached.probe(),'Weekly').used,25);assert.equal(cached.requests.length,1);
});
test('muse.ai rejects malformed successful actions without caching an ID or publishing zero',async()=>{
  const app=load('museai',{provider:{cookieHeader:'session=fixture'},files:{[actionPath]:JSON.stringify({actionID:action})},request:()=>response('1:{"success":true,"subscription":{}}')});
  await assert.rejects(app.probe,/Unexpected muse.ai response/);assert.equal(app.requests.length,1);
});
test('Muse Code binds a blank browser email through matching team membership',async()=>{
  for(const matches of [true,false]){
    const app=load('muse',{provider:{apiKey:'dca:fixture',cookieHeader:'session=fixture',workspaceId:'7'},request:req=>{
      if(req.url.endsWith('/muse-code/key'))return response({is_subs_active:true,subs_tier_name:'Pro',user_email:'fixture@example.test'});
      if(req.url.endsWith('/auth/me'))return response({email:'',userId:42});
      if(req.url.endsWith('/teams'))return response({teams:[{team_id:7,team_name:'Fixture team'}]});
      if(req.url.endsWith('/members'))return response({members:[{user_id:42,email:matches?'fixture@example.test':'another@example.test'}]});
      return response({subscription_quota:{tier:'Pro',weekly_weighted_used:'20',weekly_weighted_limit:'100',weekly_resets_at:1790000000,
        window_duration_secs:18000,window_weighted_used:0,window_weighted_limit:100}});
    }});
    const result=await app.probe();assert.equal(metric(result,'Weekly')?.used,matches?20:undefined);
    if(!matches)assert.ok(!app.requests.some(req=>req.url.endsWith('/subscription-quota')));
    assert.ok(app.requests.length<=6);
  }
});

function sqliteFixture(t,sql){
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'usagestat-opencode2-')),db=path.join(dir,'opencode-next.db');
  t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));
  const run=(script,args=[])=>{const result=spawnSync(process.platform==='win32'?'python':'python3',['-c',script,...args],{encoding:'utf8'});assert.equal(result.status,0,result.stderr);return result.stdout;};
  run('import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.close()',[db,sql]);
  return{db,read:(selected,query)=>{assert.equal(selected,db);return run('import sqlite3,json,sys; c=sqlite3.connect("file:"+sys.argv[1]+"?mode=ro",uri=True); c.row_factory=sqlite3.Row; print(json.dumps([dict(r) for r in c.execute(sys.argv[2])]))',[db,query]);}};
}
test('OpenCode 2 selected channel reads current credentials and deduplicates migrated messages',t=>{
  const data=JSON.stringify({model:{providerID:'opencode-go'},cost:1.25,tokens:{input:10,output:5,reasoning:3,cache:{read:2,write:1}}});
  const old=JSON.stringify({providerID:'opencode-go',role:'assistant',cost:1.25,tokens:{input:10,output:5,reasoning:3,cache:{read:2,write:1}}});
  const db=sqliteFixture(t,`CREATE TABLE credential(id TEXT,integration_id TEXT,active INTEGER,time_updated INTEGER,time_created INTEGER,value TEXT);
    INSERT INTO credential VALUES('old','opencode-go',0,10,1,'{"key":"inactive-fixture"}'),('new','opencode-go',1,20,2,'{"key":"active-fixture"}');
    CREATE TABLE message(id TEXT,time_created INTEGER,data TEXT);CREATE TABLE session_message(id TEXT,time_created INTEGER,type TEXT,data TEXT);
    INSERT INTO message VALUES('migrated',1788580800000,'${old}');INSERT INTO session_message VALUES('migrated',1788580800000,'assistant','${data}');
    INSERT INTO session_message VALUES('pending',1788580800000,'compaction','${data}');
    INSERT INTO session_message VALUES('completed',1788580800000,'compaction','${data.slice(0,-1)},"status":"completed"}');`);
  const app=load('opencode-go',{platform,provider:{settings:{databasePath:db.db}},files:{[db.db]:'exists'},sqlite:db.read,request:req=>{
    assert.equal(req.headers.Authorization,'Bearer active-fixture');return response({usage:{rolling:{percent:20}}});
  }});
  assert.equal(metric(app.probe(),'Session').used,20);assert.equal(app.ingested[0].daily[0].costUsd,2.5);
  assert.equal(app.ingested[0].daily[0].totalTokens,36,'Reasoning is not counted twice');
});
test('OpenCode 2 logout and unreadable storage cannot revive the stale auth import',t=>{
  const db=sqliteFixture(t,"CREATE TABLE credential(id TEXT,integration_id TEXT,active INTEGER,time_updated INTEGER,time_created INTEGER,value TEXT);");
  const files={[db.db]:'exists','/test/home/.local/share/opencode/auth.json':'{"opencode-go":{"key":"stale-private"}}'};
  const app=load('opencode-go',{platform,source:'api',provider:{settings:{databasePath:db.db}},files,sqlite:db.read});
  assert.throws(app.probe,/API key missing/);assert.equal(app.requests.length,0);
  const blocked=load('opencode-go',{platform,source:'api',provider:{settings:{databasePath:db.db}},files,sqlite:()=>{throw Error('private sqlite failure');}});
  assert.throws(blocked.probe,error=>error.code==='credential-unavailable'&&!error.message.includes('private'));assert.equal(blocked.requests.length,0);
});
