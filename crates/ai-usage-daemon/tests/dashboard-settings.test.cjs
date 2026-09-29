const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const settings=require('../src/dashboard-settings.js');
const trends=require('../src/dashboard-trends.js');

test('existing dashboard preferences migrate without importing credentials or Bar settings',()=>{
  const prefs=settings.normalize(JSON.parse('{"quotaDisplay":"remaining","inactive":"show","trends":{"range":"month","token":"secret"},"apiKey":"secret","providers":{"codex":{"visible":false,"name":" Work ","cookieHeader":"secret"},"__proto__":{"polluted":true}},"bar":{"theme":"light"}}'));
  assert.equal(prefs.theme,'dark');
  assert.equal(prefs.refreshSeconds,30);
  assert.equal(prefs.quotaDisplay,'remaining');
  assert.equal(prefs.inactive,'show');
  assert.deepEqual(prefs.providers.codex,{visible:false,name:'Work',primaryQuota:''});
  assert.deepEqual(prefs.trends,{range:'month'});
  assert.doesNotMatch(JSON.stringify(prefs),/secret|polluted|cookieHeader|apiKey|bar/);
  assert.equal({}.polluted,undefined);
});

test('imported preferences are bounded and unsupported values use defaults',()=>{
  const prefs=settings.normalize({theme:'solarized',accent:'<script>',density:'huge',refreshSeconds:0,
    icons:'false',summary:false,providerOrder:['codex','codex','__proto__','claude'],
    providers:{codex:{name:'x'.repeat(100),primaryQuota:'y'.repeat(200)}}});
  assert.equal(prefs.theme,'dark');assert.equal(prefs.accent,'blue');
  assert.equal(prefs.density,'comfortable');assert.equal(prefs.refreshSeconds,30);
  assert.equal(prefs.icons,true);assert.equal(prefs.summary,false);
  assert.deepEqual(prefs.providerOrder,['codex','claude']);
  assert.equal(prefs.providers.codex.name.length,80);
  assert.equal(prefs.providers.codex.primaryQuota.length,128);
  for(const value of [null,[],false,'text'])assert.throws(()=>settings.normalize(value));
});

test('provider ordering retains new providers and never mutates API responses',()=>{
  const items=[{id:'new'},{id:'codex'},{id:'claude'},{id:'another-new'}];
  const prefs=settings.normalize({providerOrder:['claude','retired','codex']});
  assert.deepEqual(settings.order(items,prefs,p=>p.id).map(p=>p.id),['claude','codex','new','another-new']);
  assert.equal(items[0].id,'new');
});

const snapshot={providerId:'codex',displayName:'Codex',source:'local',metrics:[
  {type:'progress',label:'Session',used:10,limit:100,format:{kind:'percent'}},
  {type:'progress',label:'Weekly',used:85,limit:100,format:{kind:'percent'}},
  {type:'text',label:'Unavailable',value:'Unknown'},
]};
test('a selected summary quota uses measured values and falls back when absent',()=>{
  const prefs=settings.normalize({providers:{codex:{primaryQuota:'Weekly'}}});
  assert.equal(settings.primary(snapshot,prefs).used,85);
  assert.equal(settings.primary({...snapshot,metrics:snapshot.metrics.slice(0,1)},prefs).used,10);
  assert.equal(settings.primary({...snapshot,metrics:[]},prefs),null);
});

function dashboard(prefs={}) {
  const html=fs.readFileSync(require.resolve('../src/dashboard.html'),'utf8');
  const script=[...html.matchAll(/<script>([\s\S]*?)<\/script>/g)][0][1].replace(/load\(\);\s*$/,'');
  const elements=new Map(),writes=[],events={};
  const element=id=>{
    if(!elements.has(id))elements.set(id,{id,style:{},dataset:{},innerHTML:'',
      querySelector:selector=>element(selector),querySelectorAll:()=>[],contains:()=>false,
      addEventListener:(event,fn)=>{element(id)[event]=fn;}});
    return elements.get(id);
  };
  const root={dataset:{},style:{setProperty(key,value){this[key]=value;}}};
  const context=vm.createContext({UsageDashboardSettings:settings,UsageTrends:trends,snapshot,
    setInterval:()=>0,clearInterval:()=>{},matchMedia:()=>({matches:true}),
    localStorage:{getItem:()=>JSON.stringify(prefs),setItem:(key,value)=>writes.push({key,value})},
    window:{addEventListener:(event,callback)=>{events[event]=callback;}},
    document:{documentElement:root,getElementById:element,querySelectorAll:()=>[]}});
  vm.runInContext(script,context);
  vm.runInContext('S.providers=[{id:"codex",name:"Codex"},{id:"claude",name:"Claude"}];S.snapshots=[snapshot,{...snapshot,providerId:"claude",displayName:"Claude"}];buildTabs=()=>{};',context);
  return{context,element,root,writes,events,run:code=>vm.runInContext(code,context)};
}

test('manual hiding stays hidden when inactive providers are shown and preserves saved history',()=>{
  const ui=dashboard({inactive:'show',providers:{codex:{visible:false}}});
  ui.run('S.dailyRows=[{providerId:"codex",date:"2026-09-30",totalTokens:7}];');
  assert.equal(ui.run('visibleSnapshots().length'),1);
  assert.equal(ui.run('visibleSnapshots()[0].providerId'),'claude');
  assert.equal(ui.run('S.dailyRows[0].totalTokens'),7);
  assert.equal(ui.run('S.providers.length'),2);
});

test('Overview summary uses selected quotas and aliases, and hidden providers retain their History',()=>{
  const ui=dashboard({providers:{codex:{name:'Work',primaryQuota:'Weekly'}}});
  ui.run('renderOverviewSummary()');
  assert.match(ui.element('overviewSummary').innerHTML,/Work has the tightest quota/);
  const date=new Date().toISOString().slice(0,10);
  ui.context.savedRows=[{providerId:'codex',date,cost:99,totalTokens:99},{providerId:'claude',date,cost:1,totalTokens:1}];
  ui.run('S.dailyRows=savedRows;S.prefs.providers.codex.visible=false;renderOverviewSummary()');
  assert.match(ui.element('overviewSummary').innerHTML,/visible providers/);
  assert.match(ui.element('overviewSummary').innerHTML,/\$1\.00/);
  assert.doesNotMatch(ui.element('overviewSummary').innerHTML,/\$99\.00|\$100\.00|Work/);
  assert.equal(ui.run('S.dailyRows.length'),2);
  assert.equal(ui.run('providerDisplayName("codex","Codex")'),'Work');
});

test('appearance, aliases and primary quotas persist only in the dashboard namespace',()=>{
  const ui=dashboard();
  ui.run('S.active="settings";setDashboardPref("theme","system");updateProviderSetting("codex","name","<Work>");updateProviderSetting("codex","primaryQuota","Weekly");');
  assert.equal(ui.root.dataset.theme,'light');
  assert.equal(ui.run('providerName("codex")'),'<Work>');
  assert.equal(ui.run('primaryPercentOf(snapshot)'),85);
  assert.ok(ui.writes.length===3&&ui.writes.every(write=>write.key===settings.STORAGE_KEY));
  const saved=JSON.parse(ui.writes.at(-1).value);
  assert.equal(saved.providers.codex.primaryQuota,'Weekly');
  assert.match(ui.element('panel-settings').innerHTML,/value="&lt;Work&gt;"/);
  assert.doesNotMatch(ui.element('panel-settings').innerHTML,/value="<Work>"/);
});

test('saving a display name preserves the form nodes needed by the next click',()=>{
  const ui=dashboard();ui.run('S.active="settings";renderSettings()');
  const before=ui.element('panel-settings').innerHTML;
  ui.run('updateProviderSetting("codex","name","Work")');
  assert.equal(ui.element('panel-settings').innerHTML,before);
  assert.equal(ui.run('providerName("codex")'),'Work');
  assert.equal(JSON.parse(ui.writes.at(-1).value).providers.codex.name,'Work');
});

test('reordering providers, resetting and undoing restore dashboard preferences',()=>{
  const ui=dashboard({theme:'light',providers:{codex:{visible:false}}});
  ui.run('S.active="settings";moveDashboardProvider("claude",-1)');
  assert.equal(ui.run('settingsProviderIds()[0]'),'claude');
  ui.run('resetDashboardSettings()');
  assert.equal(ui.run('S.prefs.theme'),'dark');
  assert.equal(ui.run('providerSetting("codex").visible'),true);
  ui.run('undoDashboardReset()');
  assert.equal(ui.run('S.prefs.theme'),'light');
  assert.equal(ui.run('providerSetting("codex").visible'),false);
  assert.equal(ui.run('settingsProviderIds()[0]'),'claude');
});

test('storage failure keeps session preferences and reports that they were not saved',()=>{
  const ui=dashboard();ui.context.localStorage.setItem=()=>{throw new Error('blocked');};
  ui.run('S.active="settings";setDashboardPref("theme","light")');
  assert.equal(ui.root.dataset.theme,'light');
  assert.equal(ui.run('S.storageError'),true);
  assert.match(ui.element('settings-status').textContent,/session only/);
});

test('settings links and another dashboard tab update the displayed page',()=>{
  const ui=dashboard();ui.context.location={hash:'#settings'};
  ui.events.hashchange();
  assert.equal(ui.run('S.active'),'settings');
  assert.match(ui.element('panel-settings').innerHTML,/Dashboard settings/);
  ui.events.storage({key:'usagestat.bar.prefs',newValue:JSON.stringify({theme:'light'})});
  assert.equal(ui.run('S.prefs.theme'),'dark');
  ui.events.storage({key:settings.STORAGE_KEY,newValue:JSON.stringify({theme:'light',providers:{codex:{visible:false}}})});
  assert.equal(ui.root.dataset.theme,'light');
  assert.equal(ui.run('visibleSnapshots().length'),1);
  assert.equal(ui.writes.length,0);
});

test('settings export and file import round-trip selected providers without credentials',async()=>{
  const ui=dashboard({theme:'light',providers:{codex:{visible:false,name:'Work',primaryQuota:'Weekly'}}});
  ui.run('S.active="settings";downloadText=(name,body)=>{exported=JSON.parse(body)};exportPreferences()');
  const exported=ui.run('exported');
  const payload={...exported,preferences:{...exported.preferences,theme:'dark',cookieHeader:'secret'}};
  const target={files:[{size:1000,text:async()=>JSON.stringify(payload)}],value:'selected.json'};
  await ui.element('prefs-file').change({target});
  assert.equal(ui.run('S.prefs.theme'),'dark');
  assert.equal(ui.run('providerSetting("codex").visible'),false);
  assert.equal(ui.run('providerSetting("codex").name'),'Work');
  assert.doesNotMatch(ui.writes.at(-1).value,/secret|cookieHeader/);
  assert.equal(target.value,'');
  target.files=[{size:1000,text:async()=>JSON.stringify({...payload,version:99})}];
  await ui.element('prefs-file').change({target});
  assert.match(ui.element('settings-status').textContent,/Unsupported preferences version/);
  assert.equal(ui.run('S.prefs.theme'),'dark');
});
