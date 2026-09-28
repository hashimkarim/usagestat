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

test('daemon serves compact history and saved model days with source and price coverage', {timeout:30000}, async t=>{
  assert.ok(fs.existsSync(binary), 'Build with cargo build -p usagestat-daemon before this test');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(),'usagestat-history-test-'));
  const data=path.join(dir,'data'), profile=path.join(dir,'codex'), plugins=path.join(dir,'plugins');
  for(const sub of [data,path.join(profile,'sessions'),path.join(plugins,'codex')]) fs.mkdirSync(sub,{recursive:true});
  const day='2026-09-01';
  const usage=(ts,input,cached,output,reasoning)=>({type:'event_msg',timestamp:ts,payload:{type:'token_count',info:{
    total_token_usage:{input_tokens:input,cached_input_tokens:cached,output_tokens:output,reasoning_output_tokens:reasoning},
    last_token_usage:{input_tokens:input,cached_input_tokens:cached,output_tokens:output,reasoning_output_tokens:reasoning},
  }}});
  const first=usage(day+'T23:59:00Z',100,80,10,4);
  const log=[{type:'session_meta',payload:{id:'real-session'}},{type:'turn_context',payload:{model:'gpt-6-astra'}},
    first,first,{type:'response_item',payload:{id:'message-not-session'}},usage('2026-09-02T00:01:00Z',150,120,20,8),
    {type:'turn_context',payload:{model:'unknown'}},usage('2026-09-02T00:02:00Z',180,140,25,10),
    {type:'turn_context',payload:{model:'gpt-6-astra'}},usage('2026-09-03T00:00:00Z',200,150,35,12)];
  fs.writeFileSync(path.join(profile,'sessions','fixture.jsonl'),log.map(JSON.stringify).join('\n')+'\n');
  fs.writeFileSync(path.join(plugins,'codex','plugin.json'),JSON.stringify({id:'codex',name:'Codex fixture',entry:'plugin.js',enabledByDefault:true,supportedModes:['local'],autoMode:'local'}));
  fs.writeFileSync(path.join(plugins,'codex','plugin.js'),'globalThis.__usagestat_plugin={probe:()=>({displayName:"Codex fixture",source:"local",lines:[]})};');
  const claudeProject=path.join(dir,'claude/projects/fixture');
  fs.mkdirSync(claudeProject,{recursive:true});
  fs.mkdirSync(path.join(plugins,'claude'),{recursive:true});
  fs.writeFileSync(path.join(plugins,'claude','plugin.json'),JSON.stringify({id:'claude',name:'Claude fixture',entry:'plugin.js',enabledByDefault:true,supportedModes:['local'],autoMode:'local'}));
  fs.writeFileSync(path.join(plugins,'claude','plugin.js'),'globalThis.__usagestat_plugin={probe:()=>({source:"local",lines:[]})};');
  const claudeEvent={timestamp:day+'T12:00:00Z',sessionId:'claude-session',requestId:'request',message:{id:'message',model:'claude-opus-5-5',
    usage:{input_tokens:2,output_tokens:10,cache_read_input_tokens:100,cache_creation_input_tokens:20}}};
  const nextClaudeEvent={...claudeEvent,timestamp:'2026-09-02T12:00:00Z',requestId:'next-request'};
  fs.writeFileSync(path.join(claudeProject,'session.jsonl'),[claudeEvent,{...claudeEvent,timestamp:day+'T12:00:01Z'},nextClaudeEvent,{...nextClaudeEvent,timestamp:'2026-09-02T12:00:01Z'}].map(JSON.stringify).join('\n'));
  fs.writeFileSync(path.join(dir,'config.toml'),'refreshSec = 3600\n');
  const billingRow={providerId:'codex',displayName:'Codex',date:day,source:'billing',
    inputTokens:20,outputTokens:10,cacheReadTokens:80,cacheCreationTokens:0,reasoningOutputTokens:4,totalTokens:114,costUsd:12,ingestedAt:'first'};
  fs.writeFileSync(path.join(data,'usage_daily.json'),JSON.stringify({version:1,rows:[billingRow,
    {...billingRow,source:'ccusage',inputTokens:15,outputTokens:20,cacheReadTokens:70,cacheCreationTokens:15,
      totalTokens:120,costUsd:2,costKnown:false,tokensKnown:false},
    {...billingRow,providerId:'claude',displayName:'Claude'},
    {...billingRow,providerId:'claude',displayName:'Claude',source:'ccusage',costUsd:8},
    {...billingRow,providerId:'claude',displayName:'Claude',date:'2026-09-02',source:'ccusage',costUsd:8},
    {...billingRow,date:'2026-09-02',source:'ccusage',costUsd:99},
    {...billingRow,date:'2026-09-04',source:'ccusage',costUsd:4},
  ]}));
  const snapshots=Array.from({length:100},(_,i)=>({ts:day+`T09:${String(i%60).padStart(2,'0')}:00Z`,providerId:'codex',displayName:'Codex',
    primaryPercent:i===0?90:10,cost:i===59?2:10,progress:[{label:'Quota',used:i===0?90:10,limit:100,format:'percent'}],
    charts:[{label:'Heavy chart',points:Array.from({length:100},(_,j)=>({label:String(j),value:j}))}]}));
  fs.writeFileSync(path.join(data,'history.jsonl'),snapshots.map(JSON.stringify).join('\n')+'\n');
  const listener=net.createServer(); listener.listen(0,'127.0.0.1'); await once(listener,'listening');
  const port=listener.address().port; await new Promise(resolve=>listener.close(resolve));
  const child=spawn(binary,['--bind',`127.0.0.1:${port}`,'--config',path.join(dir,'config.toml'),'--plugin-dir',plugins],{
    cwd:dir,env:{...process.env,USAGESTAT_DATA_DIR:data,USAGESTAT_CONFIG_DIR:path.join(dir,'config'),CODEX_HOME:profile,CLAUDE_CONFIG_DIR:path.join(dir,'claude')},stdio:['ignore','ignore','pipe'],
  });
  let errors=''; child.stderr.on('data',chunk=>{errors+=chunk;});
  t.after(async()=>{child.kill(); if(child.exitCode===null&&child.signalCode===null) await once(child,'exit');fs.rmSync(dir,{recursive:true,force:true});});
  const base=`http://127.0.0.1:${port}`;
  const get=async endpoint=>{const response=await fetch(base+endpoint);assert.equal(response.status,200,endpoint);return response.json();};
  for(let i=0;i<100;i++){
    try {if((await fetch(base+'/health')).ok) break;} catch {}
    assert.equal(child.exitCode,null,errors); await delay(50);
  }
  let models;
  for(let i=0;i<100;i++){
    models=await get('/v1/history/models/codex'); if(models.daily.length===3) break; await delay(50);
  }
  assert.equal(models.daily.length,3,errors);
  let claudeModels;
  for(let i=0;i<100;i++){
    claudeModels=await get('/v1/history/models/claude'); if(claudeModels.daily.length===2) break; await delay(50);
  }
  assert.equal(claudeModels.daily[0].totalTokens,132,'repeated Claude records count once');
  assert.equal(claudeModels.daily[0].timeZone,'UTC');

  const days=Object.fromEntries(models.daily.map(row=>[row.date,row]));
  assert.equal(days[day].totalTokens,110);
  assert.equal(days[day].sessions,1);
  assert.equal(days[day].models['gpt-6-astra'].costKnown,true);
  assert.equal(days[day].costSource,'api-rate-estimate');
  assert.equal(days[day].pricingAsOf,'2026-09-27');
  assert.equal(days['2026-09-02'].totalTokens,95);
  assert.equal(days['2026-09-02'].models.unknown.costKnown,false);
  assert.equal(days['2026-09-02'].costKnown,false);
  assert.ok(days[day].cacheSavingsUsd>0);
  const componentFields=['inputCostUsd','cacheReadCostUsd','cacheWriteCostUsd','outputCostUsd'];
  for(const row of [days[day],days[day].models['gpt-6-astra'],days['2026-09-03']]){
    assert.ok(componentFields.every(key=>Number.isFinite(row[key])));
    assert.ok(Math.abs(componentFields.reduce((sum,key)=>sum+row[key],0)-row.costUsd)<1e-9);
  }
  assert.equal(days[day].inputCostUsd,0.0002);
  assert.equal(days[day].cacheReadCostUsd,0.00008);
  assert.equal(days[day].cacheWriteCostUsd,0);
  assert.equal(days[day].outputCostUsd,0.0005);
  assert.ok(componentFields.every(key=>!(key in days['2026-09-02'])),'mixed unpriced day has no complete cost stack');
  assert.ok(componentFields.every(key=>!(key in days['2026-09-02'].models.unknown)));
  const daily=await get('/v1/history/daily/codex');
  assert.equal(daily.daily.find(row=>row.date===day).costUsd,12,'billing remains authoritative');
  assert.equal(daily.daily.find(row=>row.date===day).totalTokens,110,'legacy reasoning double-count is repaired');
  assert.ok(componentFields.every(key=>!(key in daily.daily.find(row=>row.date===day))),'billing does not inherit transcript components');
  assert.equal(daily.daily.find(row=>row.date==='2026-09-03').outputCostUsd,0.0005);
  assert.equal(daily.daily.find(row=>row.date==='2026-09-02').source,'local-transcript-v2');
  assert.equal(daily.daily.find(row=>row.date==='2026-09-02').costKnown,false,'local partial pricing stays explicit rather than borrowing ccusage cost');
  assert.equal(daily.daily.find(row=>row.date==='2026-09-04').source,'ccusage','ccusage-only days remain selected');
  const allDaily=await get('/v1/history/daily');
  assert.ok(allDaily.daily.every(row=>!('sourceRows' in row)),'default shape is unchanged');
  assert.deepEqual(await get('/v1/history/daily?includeSources=false'),allDaily);
  assert.deepEqual(await get('/v1/history/daily/codex?includeSources=false'),daily);
  const compared=await get('/v1/history/daily/codex?includeSources=true');
  const allCompared=await get('/v1/history/daily?includeSources=true');
  const stripSources=report=>({...report,daily:report.daily.map(({sourceRows,...row})=>row)});
  assert.deepEqual(stripSources(compared),daily,'selected rows are unchanged');
  assert.deepEqual(stripSources(allCompared),allDaily);
  assert.deepEqual(allCompared.daily.filter(row=>row.providerId==='codex'),compared.daily);
  assert.equal(allCompared.daily.find(row=>row.providerId==='claude').sourceRows.length,3);
  assert.ok(compared.daily.filter(row=>['2026-09-03','2026-09-04'].includes(row.date)).every(row=>!('sourceRows' in row)),'single-source days omit alternatives');
  const claudeSources=allCompared.daily.find(row=>row.providerId==='claude').sourceRows;
  assert.equal(claudeSources.find(row=>row.selected).source,'billing');
  assert.equal(claudeSources.find(row=>row.source==='billing').costUsd,12);
  assert.equal(claudeSources.find(row=>row.source==='ccusage').costUsd,8);
  assert.ok(claudeSources.filter(row=>row.source!=='local-transcript-v2').every(row=>row.ingestedAt==='first'&&!('timeZone' in row)));
  const selectedClaude=allCompared.daily.find(row=>row.providerId==='claude'&&row.date==='2026-09-02');
  assert.equal(selectedClaude.source,'local-transcript-v2');
  assert.equal(selectedClaude.totalTokens,132,'selected headline uses deduplicated Claude usage');
  assert.ok(Math.abs(selectedClaude.costUsd-0.000328)<1e-12);
  assert.equal(selectedClaude.sourceRows.find(row=>row.selected).source,'local-transcript-v2');
  assert.equal(selectedClaude.sourceRows.find(row=>row.source==='ccusage').costUsd,8,'retained fallback is unchanged');
  const sources=compared.daily.find(row=>row.date===day).sourceRows;
  assert.equal(sources.length,3);
  assert.equal(sources.filter(row=>row.selected).length,1);
  assert.deepEqual(sources.find(row=>row.selected),{source:'billing',selected:true,ingestedAt:'first',costUsd:12,costKnown:true,
    totalTokens:110,tokensKnown:true,inputTokens:20,cacheReadTokens:80,cacheCreationTokens:0,outputTokens:10});
  assert.deepEqual(sources.find(row=>row.source==='ccusage'),{source:'ccusage',selected:false,ingestedAt:'first',costUsd:2,costKnown:false,
    totalTokens:120,tokensKnown:false,inputTokens:15,cacheReadTokens:70,cacheCreationTokens:15,outputTokens:20});
  const local=sources.find(row=>row.costSource==='api-rate-estimate');
  assert.equal(local.pricingAsOf,'2026-09-27');
  assert.equal(local.timeZone,'UTC');
  assert.ok(Number.isFinite(Date.parse(local.ingestedAt)));
  assert.ok(sources.filter(row=>row.source!=='local-transcript-v2').every(row=>!('timeZone' in row)),'legacy rows are not assigned a zone');
  assert.equal(local.selected,false);
  assert.equal(local.costUsd,days[day].costUsd);
  assert.equal(local.totalTokens,days[day].totalTokens);
  assert.ok(sources.every(row=>!('models' in row)&&!('inputCostUsd' in row)),'source summaries stay compact');
  const periodCompared=await get('/v1/history/daily/codex/all?includeSources=true');
  assert.deepEqual(periodCompared.daily,compared.daily);
  assert.equal((await fetch(base+'/v1/history/daily/codex/invalid?includeSources=true')).status,400);
  const cost=await get('/v1/cost/openai');
  assert.equal(cost.provider,'codex');
  assert.equal(cost.currency,'USD');
  assert.ok(cost.totals.totalTokens>0);
  assert.equal(cost.daily.find(row=>row.date==='2026-09-03').outputCostUsd,0.0005);
  assert.ok(componentFields.every(key=>!(key in cost.totals)),'mixed source totals have no complete cost stack');
  const sessions=await get('/v1/local-usage/codex/session?limit=80');
  assert.equal(sessions.totalRows,1);
  assert.equal(sessions.sessions[0].sessionId,'real-session');
  assert.equal(sessions.sessions[0].totalTokens,235);
  assert.equal(sessions.sessions[0].costKnown,false);
  const url='/v1/history/codex?since=2026-09-01&until=2026-09-01&group=hour&view=chart';
  const response=await fetch(base+url), body=await response.text(), history=JSON.parse(body);
  assert.ok(!body.includes('\n'),'JSON is compact');
  assert.equal(history.length,1);
  assert.equal(history[0].primaryPercent,90);
  assert.equal(history[0].cost,2,'counters are not summed');
  assert.equal(history[0].charts.length,0);
  assert.ok(body.length<1000,'chart response excludes heavy snapshot payloads');
  assert.equal((await fetch(base+'/v1/history/codex?since=invalid')).status,400);
  assert.equal((await fetch(base+'/v1/local-usage/codex/session?limit=0')).status,400);
  assert.deepEqual(await get('/v1/local-usage/codex/session?limit=80'),sessions,'cached reports retain all semantics');
});
