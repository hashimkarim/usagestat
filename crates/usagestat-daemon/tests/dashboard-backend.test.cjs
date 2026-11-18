const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const os=require('node:os');
const path=require('node:path');
const net=require('node:net');
const {spawn}=require('node:child_process');
const {once}=require('node:events');
const client=require('../src/dashboard-backend.js');
const root=path.resolve(__dirname,'../../..');
const binary=process.env.USAGESTAT_TEST_DAEMON||path.join(root,'target/debug/usagestatd'+(process.platform==='win32'?'.exe':''));
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function until(check){for(let i=0;i<150;i++){const value=await check();if(value)return value;await delay(100);}throw new Error('Native backend did not reach the expected state');}

test('browser collection client keeps its capability out of URLs and never retries a stale write',async()=>{
  const requests=[],stored=new Map();let stale=false;
  const store={getItem:key=>stored.get(key),setItem:(key,value)=>stored.set(key,value),removeItem:key=>stored.delete(key)};
  const api=client.create(async(url,options)=>{requests.push({url,options});const status=stale?409:200;return{ok:status===200,status,json:async()=>url.endsWith('/session')?{token:'test-capability'}:{revision:'rev',providers:[]}};},store);
  await api.load('private-test-setup-key');await api.save({refreshSec:60});stale=true;
  await assert.rejects(api.save({refreshSec:90}),/Reload/);
  assert.equal(requests.length,4);assert.ok(requests.every(r=>!r.url.includes('test-capability')));
  assert.equal(requests[2].options.headers['X-Usagestat-Session'],'test-capability');
  assert.equal(requests[0].options.headers['X-Usagestat-Setup-Key'],'private-test-setup-key');
  assert.ok(requests.slice(1).every(r=>!('X-Usagestat-Setup-Key' in r.options.headers)));
  assert.equal(stored.get(client.SESSION_KEY),'test-capability');assert.doesNotMatch(JSON.stringify([...stored]),/private-test-setup-key/);
  assert.deepEqual(JSON.parse(requests[2].options.body),{refreshSec:60,revision:'rev'});
  assert.equal(requests[2].options.credentials,'same-origin');assert.equal(requests[2].options.cache,'no-store');
  api.forget();assert.equal(api.data,null);assert.equal(stored.size,0);
  assert.deepEqual(client.modes({supportedModes:['api']}),['auto','api','custom']);
  assert.ok(client.modes({supportedModes:[]}).includes('oauth'));
  assert.throws(()=>client.settingValue('maybe','boolean'));assert.throws(()=>client.settingValue('','number'));
});

test('browser collection access survives this tab reload but clears an expired capability',async()=>{
  const stored=new Map([[client.SESSION_KEY,'previous-launch-capability']]),requests=[];
  const api=client.create(async(url,options)=>{requests.push(options);return{ok:false,status:401,json:async()=>({error:'dashboard_setup_key_required',keyFile:'/private/fixture/dashboard-setup.key'})};},{getItem:key=>stored.get(key),removeItem:key=>stored.delete(key)});
  await assert.rejects(api.load(),error=>error.status===401&&error.keyFile==='/private/fixture/dashboard-setup.key');
  assert.equal(requests[0].headers['X-Usagestat-Session'],'previous-launch-capability');assert.equal(stored.size,0);assert.equal(api.data,null);
});

test('provider setup fields preserve saved types and expose requirements without credential values',()=>{
  const rows=client.settingRows({setupFields:[{key:'browserUserAgent',title:'Browser User-Agent',description:'Use the cookie-owning browser',type:'string'},
    {key:'days',title:'Days',description:'History range',type:'number'},{key:'__proto__',type:'string'}]},
    {browserUserAgent:{configured:true,type:'string',value:'Fixture Browser/1'},accessToken:{configured:true,type:'string',secret:true}});
  assert.equal(rows.length,3);assert.equal(rows[0][1].value,'Fixture Browser/1');
  assert.equal(rows[0][1].hint,'Use the cookie-owning browser');assert.equal(rows[1][1].configured,false);
  assert.equal(rows[2][1].value,undefined);assert.equal(rows[2][1].secret,true);
});

test('native collection setup is redacted, origin protected, durable and reloads independent sources', {timeout:60000},async t=>{
  assert.ok(fs.existsSync(binary),'Build the daemon before running native dashboard tests');
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'usagestat-collection-test-'));
  const plugins=path.join(dir,'plugins'),config=path.join(dir,'config.toml'),data=path.join(dir,'data');
  fs.mkdirSync(path.join(plugins,'fixture'),{recursive:true});fs.mkdirSync(data,{recursive:true});
  const settings='refreshSec = 3600\nfutureRoot = "keep-root"\n[[providers]]\nid = "fixture"\nenabled = true\nsource = "api"\napiKey = "saved-fixture-key"\ncookieHeader = "saved-fixture-cookie"\ncustomCommand = "saved-fixture-command"\nfutureProvider = "keep-provider"\n[providers.settings]\nquota = 11\nregion = "eu"\naccessToken = "saved-fixture-setting"\ncomplex = { nested = "preserve" }\n';
  fs.writeFileSync(config,settings,{mode:0o600});
  const setupFields=[{key:'browserUserAgent',title:'Browser User-Agent',description:'Use the same browser as the cookies',type:'string'}];
  fs.writeFileSync(path.join(plugins,'fixture','plugin.json'),JSON.stringify({id:'fixture',name:'Fixture provider',entry:'plugin.js',enabledByDefault:true,supportedModes:['api','local'],autoMode:'local',setupFields,icon:path.join(root,'plugins/copilot/icon.svg')}));
  fs.writeFileSync(path.join(plugins,'fixture','plugin.js'),`globalThis.__usagestat_plugin={probe(ctx){const n=Number(ctx.provider.settings.quota||0);ctx.host.usageDaily.ingest({source:'fixture',daily:[{date:'2026-09-30',totalTokens:n,inputTokens:n,outputTokens:0,costUsd:0,tokensKnown:true,costKnown:false}]});return {source:ctx.sourceMode,lines:[{type:'progress',label:'Quota',used:n,limit:100,format:{kind:'percent'}}]};}};`);
  const allocate=net.createServer();allocate.listen(0,'127.0.0.1');await once(allocate,'listening');const port=allocate.address().port;await new Promise(r=>allocate.close(r));
  const base=`http://127.0.0.1:${port}`;let child;
  const start=async extra=>{child=spawn(binary,['--bind',`127.0.0.1:${port}`,'--config',config,'--plugin-dir',plugins,...(extra||[])],{cwd:dir,stdio:['ignore','ignore','pipe'],env:{PATH:process.env.PATH,HOME:dir,...(process.env.SystemRoot?{SystemRoot:process.env.SystemRoot}:{}),USAGESTAT_CONFIG_DIR:path.join(dir,'config'),USAGESTAT_DATA_DIR:data}});child.stderr.resume();await until(async()=>{try{return (await fetch(base+'/v1/providers')).ok;}catch{return false;}});};
  const stop=async()=>{if(child&&child.exitCode===null){const exited=once(child,'exit');child.kill('SIGTERM');await exited;}};
  t.after(async()=>{await stop();fs.rmSync(dir,{recursive:true,force:true});});
  await start();
  let token,view;
  const request=(route='/v1/settings',method='GET',body,headers={})=>fetch(base+route,{method,headers:{'X-Usagestat-Dashboard':'1',...(token?{'X-Usagestat-Session':token}:{}),...(method==='PATCH'?{'Content-Type':'application/json',Origin:base}:{}),...headers},...(body?{body:JSON.stringify(body)}:{})});
  const load=async()=>{const r=await request();assert.equal(r.status,200);view=await r.json();return view;};
  const save=async patch=>{const r=await request('/v1/settings','PATCH',{revision:view.revision,...patch});assert.equal(r.status,200);view=await r.json();return view;};
  const get=async route=>(await fetch(base+route)).json();
  assert.equal((await fetch(base+'/v1/settings/session')).status,403);
  assert.equal((await request('/v1/settings/session','GET',null,{Origin:'https://foreign.example'})).status,403);
  const rawGet=async headers=>{const socket=net.createConnection({host:'127.0.0.1',port});await once(socket,'connect');let response='';socket.on('data',chunk=>response+=chunk);socket.write('GET /v1/settings/session HTTP/1.1\r\n'+headers+'\r\n\r\n');await once(socket,'close');return response;};
  assert.match(await rawGet(`Host: attacker.example:${port}\r\nX-Usagestat-Dashboard: 1`),/HTTP\/1.1 403/);
  assert.equal((await request('/v1/settings/session','GET',null,{'Sec-Fetch-Site':'cross-site'})).status,403);
  const denied=await request('/v1/settings/session');assert.equal(denied.status,401);
  const deniedBody=await denied.json();assert.equal(deniedBody.token,undefined);assert.equal(deniedBody.keyFile,path.join(data,'dashboard-setup.key'));
  assert.equal((await request('/v1/settings/session','GET',null,{'X-Usagestat-Setup-Key':'management-or-sdk-key'})).status,401);
  const setupKey=fs.readFileSync(path.join(data,'dashboard-setup.key'),'utf8').trim();
  if(process.platform!=='win32')assert.equal(fs.statSync(path.join(data,'dashboard-setup.key')).mode&0o777,0o600);
  const sessionResponse=await request('/v1/settings/session','GET',null,{'X-Usagestat-Setup-Key':setupKey});assert.equal(sessionResponse.status,200);assert.equal(sessionResponse.headers.get('access-control-allow-origin'),null);token=(await sessionResponse.json()).token;
  assert.doesNotMatch(JSON.stringify(await (await request('/v1/settings/session')).json()),new RegExp(setupKey));
  assert.equal((await request('/v1/settings','GET',null,{'X-Usagestat-Session':'management-or-sdk-key'})).status,401);
  await load();assert.doesNotMatch(JSON.stringify(view),/saved-fixture-|keep-provider|keep-root|nested/);
  assert.deepEqual(view.catalog.find(p=>p.id==='fixture').setupFields,setupFields);
  assert.equal(view.providers[0].apiKeyConfigured,true);assert.equal(view.providers[0].settings.accessToken.secret,true);
  assert.equal(view.providers[0].settings.region.value,'eu');assert.equal(view.providers[0].settings.complex.secret,true);
  const original=fs.readFileSync(config,'utf8');
  assert.equal((await request('/v1/settings','PATCH',{revision:view.revision,refreshSec:30},{Origin:''})).status,403);
  assert.equal((await request('/v1/settings','PATCH',{revision:view.revision,provider:{id:'fixture',source:'oauth'}})).status,400);
  assert.equal((await request('/v1/settings','PATCH',{revision:view.revision,provider:{id:'fixture',instanceId:'fixture'}})).status,400);
  assert.equal((await request('/v1/settings','PATCH',{revision:view.revision,provider:{id:'fixture',settings:JSON.parse('{"__proto__":"bad"}')}})).status,400);
  assert.equal((await request('/v1/settings','PATCH',{revision:view.revision,removeProvider:'fixture'})).status,400);
  assert.equal(fs.readFileSync(config,'utf8'),original);
  // Duplicate origin/header ambiguity cannot bypass the local boundary.
  const socket=net.createConnection({host:'127.0.0.1',port});await once(socket,'connect');let response='';socket.on('data',chunk=>response+=chunk);socket.end(`GET /v1/settings/session HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nX-Usagestat-Dashboard: 1\r\nOrigin: ${base}\r\nOrigin: https://foreign.example\r\n\r\n`);await once(socket,'close');assert.match(response,/HTTP\/1.1 403/);
  await until(async()=>{const rows=await get('/v1/usage');return rows.find(r=>r.providerId==='fixture')?.metrics[0]?.used===11;});
  const stale=view.revision;await save({provider:{id:'fixture',displayName:'Renamed',settings:{quota:27}}});
  const persisted=fs.readFileSync(config,'utf8');assert.match(persisted,/saved-fixture-key/);assert.match(persisted,/keep-root/);assert.match(persisted,/keep-provider/);assert.match(persisted,/nested/);
  assert.equal((await request('/v1/settings','PATCH',{revision:stale,refreshSec:60})).status,409);
  await until(async()=>{const rows=await get('/v1/usage');return rows.find(r=>r.providerId==='fixture')?.metrics[0]?.used===27;});
  await save({provider:{id:'fixture',instanceId:'fixture-work',displayName:'Work',source:'api',enabled:true,apiKey:'different-fixture-key',settings:{quota:64}}});
  await until(async()=>{const rows=await get('/v1/usage');return rows.find(r=>r.providerId==='fixture-work')?.metrics[0]?.used===64;});
  const providers=await get('/v1/providers');assert.equal(providers.find(p=>p.id==='fixture-work').pluginId,'fixture');
  const daily=(await get('/v1/history/daily')).daily;assert.equal(daily.find(r=>r.providerId==='fixture').totalTokens,27);assert.equal(daily.find(r=>r.providerId==='fixture-work').totalTokens,64);
  await save({provider:{id:'fixture',enabled:false,apiKey:null,settings:{accessToken:null}}});
  assert.equal(view.providers.find(p=>p.id==='fixture').apiKeyConfigured,false);
  assert.ok(!('accessToken' in view.providers.find(p=>p.id==='fixture').settings));
  assert.equal((await get('/v1/providers')).find(p=>p.id==='fixture').enabled,false);
  assert.ok((await get('/v1/usage')).some(r=>r.providerId==='fixture'),'Pausing collection retains saved usage');
  // External Bar/preferences writes are detected and not overwritten.
  fs.appendFileSync(config,'\nexternalRoot = "keep-external"\n');
  assert.equal((await request('/v1/settings','PATCH',{revision:view.revision,refreshSec:60})).status,409);await load();
  const beforeReload=(await get('/v1/usage')).find(r=>r.providerId==='fixture-work').fetchedAt;
  fs.writeFileSync(config,fs.readFileSync(config,'utf8').replace('quota = 64','quota = 73'));
  await until(async()=>{const snap=(await get('/v1/usage')).find(r=>r.providerId==='fixture-work');return snap?.metrics[0]?.used===73&&snap.fetchedAt!==beforeReload;});
  await load();await save({removeProvider:'fixture-work'});assert.ok(!view.providers.some(p=>p.instanceId==='fixture-work'));
  assert.ok((await get('/v1/history/daily')).daily.some(r=>r.providerId==='fixture-work'),'Deleting a source does not delete accounting history');
  if(process.platform!=='win32'){
    // Custom usage commands execute only explicitly saved local readout code.
    const body={source:'error',state:'failed',fetchedAt:'2026-09-01T00:00:00Z',metrics:[{type:'progress',label:'Retained quota',used:19,limit:100,format:{kind:'percent'}}]};
    await save({provider:{id:'fixture',source:'custom',enabled:true,customCommand:"printf '%s' '"+JSON.stringify(body)+"'"}});
    await until(async()=>{const snap=(await get('/v1/usage')).find(r=>r.providerId==='fixture');return snap?.state==='failed'&&snap.metrics[0]?.used===19&&snap.fetchedAt==='2026-09-01T00:00:00Z';});
    const historical=await get('/v1/history/fixture?view=chart');assert.ok(historical.some(r=>r.state==='failed'&&r.source==='custom'&&r.ts.startsWith('2026-09-01')));
    await save({provider:{id:'fixture',customCommand:"printf '%s' 'private-fixture-output'; exit 1"}});
    await until(async()=>{const snap=(await get('/v1/usage')).find(r=>r.providerId==='fixture');return snap?.source==='error'&&!JSON.stringify(snap).includes('private-fixture-output');});
    await save({provider:{id:'fixture',enabled:false}});
  }
  const icons=await get('/v1/icon-catalog');assert.ok(icons.icons.length>=150);assert.doesNotMatch(JSON.stringify(icons),/\.svg|\/mnt\//);
  for(const style of ['color','monochrome']){const r=await fetch(base+'/v1/icons/fixture?style='+style+'&source=copilot');assert.equal(r.status,200);assert.match(await r.text(),/<svg/);}
  assert.equal((await fetch(base+'/v1/icons/fixture?style=color&source=../../config.toml')).status,404);
  assert.equal((await fetch(base+'/v1/icons/fixture?style=bad')).status,400);
  if(process.platform!=='win32')assert.equal(fs.statSync(config).mode&0o777,0o600);
  const oldToken=token;await stop();await start();assert.equal((await request()).status,401,'Capabilities rotate on daemon restart');
  assert.equal((await request('/v1/settings/session')).status,401);
  assert.equal(fs.readFileSync(path.join(data,'dashboard-setup.key'),'utf8').trim(),setupKey,'The private profile key survives restart');
  token=(await (await request('/v1/settings/session','GET',null,{'X-Usagestat-Setup-Key':setupKey})).json()).token;assert.notEqual(token,oldToken);await load();assert.equal(view.providers[0].enabled,false);
  await stop();await start(['--no-poll']);assert.equal((await request('/v1/settings/session')).status,404,'Ingestion-only daemons never expose setup');assert.deepEqual(await get('/v1/providers'),[]);
});
