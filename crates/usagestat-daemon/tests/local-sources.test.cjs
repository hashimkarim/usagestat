const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const {spawn} = require('node:child_process');
const {once} = require('node:events');

const root = path.resolve(__dirname, '../../..');
const binary = process.env.USAGESTAT_TEST_DAEMON || path.join(root, 'target/debug/usagestatd'+(process.platform==='win32'?'.exe':''));
const delay = ms => new Promise(resolve=>setTimeout(resolve,ms));

test('Gemini CLI and Amp fixture logs become saved daily history and local reports', {timeout:30000}, async t=>{
  assert.ok(fs.existsSync(binary), 'Build with cargo build -p usagestat-daemon before this test');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(),'usagestat-local-sources-'));
  const data=path.join(dir,'data'), plugins=path.join(dir,'plugins');
  const geminiRoot=path.join(dir,'gemini-tmp'), ampRoot=path.join(dir,'amp-data');
  const chats=path.join(geminiRoot,'fixture-project','chats'), threads=path.join(ampRoot,'threads');
  for(const sub of [data,chats,threads]) fs.mkdirSync(sub,{recursive:true});
  for(const id of ['gemini','amp']){
    fs.mkdirSync(path.join(plugins,id),{recursive:true});
    fs.writeFileSync(path.join(plugins,id,'plugin.json'),JSON.stringify({id,name:id+' fixture',entry:'plugin.js',enabledByDefault:true,supportedModes:['local'],autoMode:'local'}));
    fs.writeFileSync(path.join(plugins,id,'plugin.js'),'globalThis.__usagestat_plugin={probe:()=>({source:"local",lines:[]})};');
  }
  const tokens={input:1000,output:50,cached:400,thoughts:10,tool:0,total:1060};
  const gemini=[{sessionId:'gemini-session',projectHash:'hash',startTime:'2026-09-01T00:00:00Z'},
    {id:'u',type:'user',timestamp:'2026-09-01T09:59:00Z',content:'private prompt'},
    {id:'m1',type:'gemini',timestamp:'2026-09-01T10:00:00Z',model:'gemini-2.5-pro',content:'private answer'},
    {id:'m1',type:'gemini',timestamp:'2026-09-01T10:00:00Z',model:'gemini-2.5-pro',content:'private answer',tokens},
    {id:'m2',type:'gemini',timestamp:'2026-09-02T10:00:00Z',model:'gemini-2.5-pro',tokens}];
  fs.writeFileSync(path.join(chats,'session-1.jsonl'),gemini.map(JSON.stringify).join('\n')+'\n{"truncated":');
  // A legacy document migrated to .jsonl is not counted again.
  fs.writeFileSync(path.join(chats,'session-1.json'),JSON.stringify({sessionId:'gemini-session',messages:gemini.slice(1)}));
  const ledgerEvent={id:'e1',timestamp:'2026-09-01T12:00:00Z',model:'claude-sonnet-4-5',tokens:{input:100,output:10},toMessageId:1};
  fs.writeFileSync(path.join(threads,'T-fixture.json'),JSON.stringify({id:'T-fixture',created:Date.parse('2026-09-01T11:00:00Z'),
    messages:[{role:'user',messageId:0,content:[{type:'text',text:'private prompt'}]},
      {role:'assistant',messageId:1,usage:{model:'claude-sonnet-4-5',inputTokens:100,outputTokens:10,cacheReadInputTokens:1000}}],
    usageLedger:{events:[ledgerEvent,ledgerEvent]}}));
  fs.writeFileSync(path.join(dir,'config.toml'),'refreshSec = 3600\n');
  const listener=net.createServer(); listener.listen(0,'127.0.0.1'); await once(listener,'listening');
  const port=listener.address().port; await new Promise(resolve=>listener.close(resolve));
  const child=spawn(binary,['--bind',`127.0.0.1:${port}`,'--config',path.join(dir,'config.toml'),'--plugin-dir',plugins],{
    cwd:dir,env:{...process.env,USAGESTAT_DATA_DIR:data,USAGESTAT_CONFIG_DIR:path.join(dir,'config'),
      GEMINI_DATA_DIR:geminiRoot,AMP_DATA_DIR:ampRoot,CLAUDE_CONFIG_DIR:path.join(dir,'claude')},stdio:['ignore','ignore','pipe'],
  });
  let errors=''; child.stderr.on('data',chunk=>{errors+=chunk;});
  t.after(async()=>{child.kill(); if(child.exitCode===null&&child.signalCode===null) await once(child,'exit');fs.rmSync(dir,{recursive:true,force:true});});
  const base=`http://127.0.0.1:${port}`;
  const get=async endpoint=>{const response=await fetch(base+endpoint);assert.equal(response.status,200,endpoint);return response.json();};
  for(let i=0;i<100;i++){
    try {if((await fetch(base+'/health')).ok) break;} catch {}
    assert.equal(child.exitCode,null,errors); await delay(50);
  }
  // The background summarizer saves rows for enabled providers without a request.
  let models;
  for(let i=0;i<100;i++){
    models=await get('/v1/history/models/gemini'); if(models.daily.length===2) break; await delay(50);
  }
  assert.equal(models.daily.length,2,errors);
  const day=models.daily.find(row=>row.date==='2026-09-01');
  assert.equal(day.totalTokens,1060,'repeated message versions and the migrated legacy file count once');
  assert.equal(day.cacheReadTokens,400);
  assert.equal(day.inputTokens,600);
  assert.equal(day.outputTokens,60);
  assert.equal(day.costKnown,false,'Gemini has no verified local rate');
  assert.equal(day.source,'local-transcript-v2');
  const geminiDaily=await get('/v1/history/daily/gemini');
  assert.equal(geminiDaily.daily.length,2);
  const sessions=await get('/v1/local-usage/gemini/session?limit=80');
  assert.equal(sessions.totalRows,1);
  assert.equal(sessions.sessions[0].sessionId,'gemini-session');
  assert.equal(sessions.sessions[0].project,'fixture-project');

  const amp=await get('/v1/local-usage/amp/daily');
  assert.equal(amp.daily.length,1);
  assert.equal(amp.daily[0].totalTokens,1110,'ledger event and its message are one inference');
  assert.equal(amp.daily[0].costKnown,true);
  let ampHistory;
  for(let i=0;i<100;i++){
    ampHistory=await get('/v1/history/daily/amp'); if(ampHistory.daily.length===1) break; await delay(50);
  }
  assert.equal(ampHistory.daily[0].totalTokens,1110);
  assert.equal((await get('/v1/cost/amp')).provider,'amp');

  const cacheFiles=fs.readdirSync(data).filter(name=>name.startsWith('local-usage-'));
  assert.ok(cacheFiles.some(name=>name.startsWith('local-usage-gemini-')));
  for(const name of cacheFiles) assert.ok(!fs.readFileSync(path.join(data,name),'utf8').includes('private'),'caches hold no conversation text');
});
