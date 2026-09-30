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

test('display customization imports preserve choices and reject active or oversized logos',()=>{
  const prefs=settings.normalize({iconStyle:'monochrome',iconFill:'usage',customAccent:'#123456',neutralColor:'#abcdef',providerSpacing:24,
    components:['percent','logo','percent','bad'],thresholds:[{id:'limit',percent:100,color:'#ff0011',notify:true,name:'Limit'},
      {id:'warning',percent:60,color:'#ffee11',notify:false,name:'Watch'}],providers:{codex:{pinned:true,iconSource:'openai',iconStyle:'color',customIcon:'data:image/png;base64,aGVsbG8=',hiddenMetrics:['progress:Session']},claude:{customIcon:'data:image/svg+xml;base64,PHN2Zz4='}}});
  assert.deepEqual(prefs.components,['percent','logo']);assert.equal(prefs.thresholds[0].percent,60);
  assert.equal(prefs.iconStyle,'monochrome');assert.equal(prefs.customAccent,'#123456');
  assert.equal(prefs.providers.codex.iconSource,'openai');assert.ok(!prefs.providers.claude.customIcon);
  assert.equal(settings.primary(snapshot,prefs).label,'Weekly');assert.equal(settings.metrics(snapshot,prefs).length,2);
  assert.equal(settings.order(['claude','codex'],prefs,id=>id)[0],'codex');
  assert.equal(settings.thresholdAt(99,prefs).id,'warning');assert.equal(settings.thresholdAt(100,prefs).id,'limit');
  assert.ok(!settings.normalize({providers:{codex:{customIcon:'data:image/png;base64,'+'A'.repeat(524288)}}}).providers.codex.customIcon);
});

test('quota and usage details expose exact values, provenance and independent unknowns',()=>{
  const ui=dashboard();
  ui.context.detailSnapshot={...snapshot,fetchedAt:'2026-09-30T08:00:00Z',state:'ready'};
  const quota=JSON.parse(ui.run('JSON.stringify(snapshotDetail(detailSnapshot,detailSnapshot.metrics[0]))'));
  assert.ok(quota.rows.some(([label,value])=>label==='Remaining'&&value==='90'));
  assert.ok(quota.rows.some(([label,value])=>label==='Used / remaining'&&value==='10% / 90%'));
  const missing=JSON.parse(ui.run('JSON.stringify(dailyDetail([],"Yesterday"))'));
  assert.deepEqual(missing.rows,[['Usage','No record'],['Tokens','Unknown'],['Cost','Unknown']]);
  ui.context.measured=[{providerId:'codex',date:'2026-09-30',inputTokens:100,outputTokens:10,cacheReadTokens:20,cacheCreationTokens:5,reasoningOutputTokens:4,totalTokens:135,cost:0,costKnown:false,costSource:'api-rate-estimate',pricingAsOf:'2026-09-29'}];
  const daily=JSON.parse(ui.run('JSON.stringify(dailyDetail(measured,"Today"))'));
  assert.ok(daily.rows.some(([label,value])=>label==='Input tokens'&&value==='100'));
  assert.ok(daily.rows.some(([label,value])=>label==='Cost · USD'&&value.includes('Unpriced')));
  assert.ok(daily.rows.some(([label,value])=>label==='Pricing as of'&&value==='2026-09-29'));
  assert.match(daily.note,/subscription charges/);
  const history=JSON.parse(ui.run('JSON.stringify(historyDetail(normalizeHistory([{ts:"2026-09-30T08:00:00Z",providerId:"codex",primaryPercent:20}])[0],"Earlier"))'));
  assert.ok(history.rows.some(([label,value])=>label==='Tokens / cost'&&value==='Unknown'));
  assert.ok(!history.rows.some(([label])=>label==='Cost · USD'||label==='Total tokens'));
});

test('quota threshold alerts require a new fresh observation of the same window',()=>{
  const ui=dashboard({notifications:true,thresholds:[{id:'watch',name:'Watch',percent:75,color:'#ffaa00',notify:true}]});
  const alerts=[];ui.context.Notification=function(name,options){alerts.push({name,options});};ui.context.Notification.permission='granted';
  ui.context.now=new Date(Date.now()-1000).toISOString();ui.context.later=new Date(Date.now()-500).toISOString();
  ui.run('S.snapshots=[{...snapshot,fetchedAt:now,state:"ready",metrics:[{...snapshot.metrics[0],used:70}]}];checkThresholdNotifications();S.snapshots[0].metrics[0].used=80;checkThresholdNotifications()');
  assert.equal(alerts.length,0,'Repeated timestamp is not a new observation');
  ui.run('S.snapshots[0].fetchedAt=later;checkThresholdNotifications();checkThresholdNotifications()');assert.equal(alerts.length,1);
  ui.run('S.snapshots[0].metrics[0].label="Another window";checkThresholdNotifications()');assert.equal(alerts.length,1);
  ui.run('S.snapshots[0].state="failed";S.snapshots[0].metrics[0].used=90;checkThresholdNotifications()');assert.equal(alerts.length,1);
});

test('background refresh preserves an unfocused collection draft',()=>{
  const ui=dashboard();ui.run('S.active="settings";S.settingsSection="collection";S.collectionDirty=true;');
  ui.element('panel-settings').innerHTML='Unsaved credential input';ui.run('renderSettings()');
  assert.equal(ui.element('panel-settings').innerHTML,'Unsaved credential input');
});

test('logo choices contain only the provider and its published family variants',()=>{
  const source=fs.readFileSync(require.resolve('../../../plugins/_provider-icons/manifest.js'),'utf8');
  const raw=JSON.parse(source.split('export const catalog = ')[1].trim().replace(/;$/,''));
  const catalog={aliases:raw.aliases,icons:Object.entries(raw.icons).map(([id,icon])=>({id,...icon}))};
  assert.deepEqual(settings.iconChoices(catalog,'claude').map(icon=>icon.id),['claude','anthropic','claudecode']);
  assert.deepEqual(settings.iconChoices(catalog,'gemini-cli').map(icon=>icon.id),['gemini']);
  assert.deepEqual(settings.iconChoices(catalog,'copilot').map(icon=>icon.id),['copilot','githubcopilot']);
  assert.deepEqual(settings.iconChoices(catalog,'typesafe'),[]);
  assert.ok(!settings.iconChoices(catalog,'codex').some(icon=>icon.id==='gemini'));
  const ui=dashboard({providers:{codex:{iconSource:'gemini'}}});ui.context.catalog=catalog;
  ui.run('S.iconCatalog=catalog;S.allProviders=[{id:"codex",name:"Codex",icon:{path:"codex.svg"}}];');
  assert.doesNotMatch(ui.run('providerIcon("codex")'),/source=gemini/);
  assert.match(ui.run('providerDisplayExtras("codex",snapshot,providerSetting("codex"))'),/Logo variant/);
  assert.doesNotMatch(ui.run('providerDisplayExtras("codex",snapshot,providerSetting("codex"))'),/value="gemini"/);
});

test('failed cached quotas and empty native quota records never become a measured zero',()=>{
  const ui=dashboard();
  assert.equal(ui.run('snapshotHasError({state:"failed",source:"custom",metrics:[]})'),true);
  assert.equal(ui.run('normalizeHistory([{ts:"2026-09-30T08:00:00Z",providerId:"codex",primaryPercent:0,progress:[]}])[0].primaryPercent'),null);
  assert.equal(ui.run('normalizeHistory([{ts:"2026-09-30T08:00:00Z",providerId:"codex",state:"failed",primaryPercent:80,progress:[{label:"Quota",used:80,limit:100,format:"percent"}]}])[0].primaryPercent'),null);
  const retained=JSON.parse(ui.run('JSON.stringify(historyDetail(normalizeHistory([{ts:"2026-09-30T08:00:00Z",providerId:"codex",state:"failed",primaryPercent:80,progress:[{label:"Quota",used:80,limit:100,format:"percent"}]}])[0],"Failed observation"))'));
  assert.ok(retained.rows.some(([label,value])=>label==='Quota'&&value==='80 / 100'));assert.match(retained.note,/Unavailable/);
  assert.equal(ui.run('normalizeHistory([{ts:"2026-09-30T08:00:00Z",providerId:"codex",state:"ready",primaryPercent:0,progress:[{label:"Quota",used:0,limit:100,format:"percent"}]}])[0].primaryPercent'),0);
});

test('notifications can be disabled without requesting browser permission',async()=>{
  const ui=dashboard({notifications:true});ui.run('S.active="settings";renderSettings()');
  await ui.element('#enable-notifications').click();assert.equal(ui.run('S.prefs.notifications'),false);
});

test('credit and token budgets can be selected without inventing an unlimited quota',()=>{
  const budget={providerId:'codex',metrics:[{type:'progress',label:'API credits',used:2,limit:10,format:{kind:'dollars'}},{type:'progress',label:'Unbounded spend',used:99,limit:0,format:{kind:'dollars'}}]};
  const prefs=settings.normalize({providers:{codex:{primaryQuota:'API credits'}}});
  assert.equal(settings.primary(budget,prefs).label,'API credits');
  assert.equal(settings.primary({...budget,metrics:budget.metrics.slice(1)},prefs),null);
  const ui=dashboard();assert.equal(ui.run('detailedUsd(.00032)'),'$0.00032');assert.equal(ui.run('detailedCost({cost:0,costKnown:false})'),'Unpriced');
});

test('provider readouts cannot inject HTML or executable links into setup-capable pages',async()=>{
  const ui=dashboard();ui.context.URL=URL;
  ui.run('S.snapshots=[{...snapshot,source:\'custom" onclick="bad()\',statusPageUrl:"javascript:bad()",metrics:[{type:"progress",label:"<img src=x onerror=bad()>",used:1,limit:10,format:{kind:"count",suffix:"<img src=x onerror=bad()>"}}]}];S.allProviders=[{id:"codex",usageDashboardUrl:"javascript:bad()"}];animateBarsAndArcs=()=>{};renderOverview();');
  const overview=ui.element('grid').innerHTML;
  assert.doesNotMatch(overview,/<img src=x|href="javascript:|class="badge b-custom" onclick=/);
  assert.match(overview,/&lt;img src=x onerror=bad\(\)&gt;/);
  await ui.run('animateCompareBars=()=>{};fetchHistory=async()=>[];fetchCcusageReports=async()=>null;fetchCost=async()=>null;renderStatsSection=()=>{};renderHistorySection=()=>{};renderCostSection=()=>{};renderProvider("codex")');
  assert.doesNotMatch(ui.element('panel-codex').innerHTML,/<img src=x|href="javascript:|class="badge b-custom" onclick=/);
});
