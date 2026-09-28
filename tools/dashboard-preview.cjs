#!/usr/bin/env node
// Isolated native dashboard fixture, with no real credentials or provider traffic.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawn} = require('node:child_process');
const root = path.resolve(__dirname, '..');
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'usagestat-dashboard-'));
const port = Number(process.argv[2] || 6748);
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error('Invalid port');
const providers = [['claude',21],['codex',78],['copilot',13]];
fs.mkdirSync(path.join(dir,'config'),{recursive:true});
fs.mkdirSync(path.join(dir,'data'),{recursive:true});
fs.writeFileSync(path.join(dir,'config/config.toml'),'refreshSec = 3600\n');
const now=new Date(), today=now.toISOString().slice(0,10);
for(const [provider,used] of providers) {
  const id='fixture-'+provider, plugin=path.join(dir,'plugins',id);
  fs.mkdirSync(plugin,{recursive:true});
  const name=provider[0].toUpperCase()+provider.slice(1)+' (Fixture)';
  fs.writeFileSync(path.join(plugin,'plugin.json'),JSON.stringify({id,name,entry:'plugin.js',enabledByDefault:true,
    supportedModes:['local'],autoMode:'local',icon:path.join(root,'plugins',provider,'icon.svg')}));
  fs.copyFileSync(path.join(root,'plugins',provider,'icon-color.svg'),path.join(plugin,'icon-color.svg'));
  const daily=Array.from({length:65},(_,i)=>({date:new Date(Date.parse(today)-(64-i)*86400000).toISOString().slice(0,10),
    inputTokens:1000*(i+1),outputTokens:400*(i+1),totalTokens:1400*(i+1),costUsd:(i+1)/10,
    tokensKnown:true,costKnown:provider!=='copilot',requests:i+1}));
  const snapshot={displayName:name,source:'local',plan:'Fixture',lines:[{type:'progress',label:'Weekly',used,limit:100,
    format:{kind:'percent'},resetsAt:new Date(+now+86400000).toISOString(),periodDurationMs:7*86400000}]};
  fs.writeFileSync(path.join(plugin,'plugin.js'),`globalThis.__usagestat_plugin={probe:ctx=>{ctx.host.usageDaily.ingest(${JSON.stringify({displayName:name,source:'fixture',daily})});return ${JSON.stringify(snapshot)};}};`);
}
const log=fs.openSync(path.join(dir,'daemon.log'),'a');
const child=spawn(path.join(root,'target/debug/usagestatd'),['--bind',`127.0.0.1:${port}`,'--config',path.join(dir,'config/config.toml'),
  '--plugin-dir',path.join(dir,'plugins')],{cwd:dir,detached:true,stdio:['ignore',log,log],env:{PATH:process.env.PATH,HOME:dir,
    USAGESTAT_CONFIG_DIR:path.join(dir,'config'),USAGESTAT_DATA_DIR:path.join(dir,'data')}});
child.unref();fs.closeSync(log);
console.log(JSON.stringify({url:`http://localhost:${port}/dashboard`,pid:child.pid,profile:dir,fixture:true}));
