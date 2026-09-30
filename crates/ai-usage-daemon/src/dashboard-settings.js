(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.UsageDashboardSettings = api;
})(typeof globalThis === 'object' ? globalThis : this, function () {
  'use strict';
  const STORAGE_KEY = 'usagestat.dashboard.prefs';
  const THEMES = ['dark', 'light', 'system'];
  const ACCENTS = {blue:'#7289da',green:'#43b581',purple:'#b48ead',orange:'#ce8670'};
  const SORTS = ['order', 'pressure', 'cost30', 'tokens30', 'recent', 'name'];
  const REFRESH_SECONDS = [15, 30, 60, 120, 300];
  const COMPONENTS = ['logo','name','quota','percent','pace','reset','cost'];
  const DEFAULT_THRESHOLDS = [
    {id:'warning',name:'Warning',percent:75,color:'#f6d32d',notify:false},
    {id:'danger',name:'Danger',percent:90,color:'#ff5f57',notify:false},
    {id:'limit',name:'Limit reached',percent:100,color:'#ff2d55',notify:false},
  ];
  const color = value => typeof value==='string' && /^#[0-9a-f]{6}$/i.test(value);
  const validId = id => typeof id === 'string' && /^[a-zA-Z0-9][a-zA-Z0-9_.:-]{0,127}$/.test(id) && !['constructor','prototype'].includes(id);
  function normalize(value) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid preferences.');
    const result = {
      search:'', status:'all', sort:'order', quotaDisplay:'used', inactive:'hide',
      theme:'dark', accent:'blue', density:'comfortable', icons:true,
      summary:true, refreshSeconds:30, providerOrder:[], providers:{},
      iconStyle:'color', iconFill:'full', customAccent:'', neutralColor:'',
      showPace:true, showReset:true, showStatusLinks:true, showDashboardLinks:true,
      resetTimeFormat:'smart', metricLayout:'vertical', providerSpacing:16,
      components:COMPONENTS.slice(), thresholds:DEFAULT_THRESHOLDS.map(t=>({...t})),
      notifications:false, scrollToSwitch:false, shortcuts:{overview:'o',previous:'[',next:']',refresh:'r',settings:'s'},
    };
    if (typeof value.search === 'string') result.search = value.search.slice(0, 256);
    if (['all','live','attention','near','tracked'].includes(value.status)) result.status = value.status;
    if (SORTS.includes(value.sort)) result.sort = value.sort;
    if (value.quotaDisplay === 'remaining') result.quotaDisplay = 'remaining';
    if (value.inactive === 'show') result.inactive = 'show';
    if (THEMES.includes(value.theme)) result.theme = value.theme;
    if (Object.hasOwn(ACCENTS, value.accent)) result.accent = value.accent;
    if (value.density === 'compact') result.density = 'compact';
    for (const key of ['icons','summary','showPace','showReset','showStatusLinks','showDashboardLinks','notifications','scrollToSwitch']) if (typeof value[key] === 'boolean') result[key] = value[key];
    if(['monochrome','color'].includes(value.iconStyle))result.iconStyle=value.iconStyle;
    if(['full','usage'].includes(value.iconFill))result.iconFill=value.iconFill;
    if(['smart','relative','absolute','both'].includes(value.resetTimeFormat))result.resetTimeFormat=value.resetTimeFormat;
    if(value.metricLayout==='horizontal')result.metricLayout='horizontal';
    if(Number.isInteger(value.providerSpacing)&&value.providerSpacing>=4&&value.providerSpacing<=32)result.providerSpacing=value.providerSpacing;
    for(const key of ['customAccent','neutralColor'])if(color(value[key]))result[key]=value[key];
    if(Array.isArray(value.components))result.components=[...new Set(value.components.filter(v=>COMPONENTS.includes(v)))];
    if(Array.isArray(value.thresholds)){
      const ids=new Set();
      result.thresholds=value.thresholds.slice(0,12).filter(t=>t&&validId(t.id)&&!ids.has(t.id)&&ids.add(t.id)&&Number.isFinite(t.percent)&&t.percent>=0&&t.percent<=1000&&color(t.color))
        .map(t=>({id:t.id,name:typeof t.name==='string'?t.name.trim().slice(0,80):'Threshold',percent:t.percent,color:t.color,notify:t.notify===true})).sort((a,b)=>a.percent-b.percent);
    }
    if(value.shortcuts&&typeof value.shortcuts==='object')for(const key of Object.keys(result.shortcuts)){
      if(typeof value.shortcuts[key]==='string'&&value.shortcuts[key].length<=1)result.shortcuts[key]=value.shortcuts[key].toLowerCase();
    }
    if (REFRESH_SECONDS.includes(value.refreshSeconds)) result.refreshSeconds = value.refreshSeconds;
    if (Array.isArray(value.providerOrder)) result.providerOrder = [...new Set(value.providerOrder.filter(validId))].slice(0, 256);
    if (value.providers && typeof value.providers === 'object' && !Array.isArray(value.providers)) {
      for (const [id, entry] of Object.entries(value.providers).slice(0, 256)) {
        if (!validId(id) || !entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
        const setting = {visible:true, name:'', primaryQuota:''};
        if (typeof entry.visible === 'boolean') setting.visible = entry.visible;
        if (typeof entry.name === 'string') setting.name = entry.name.trim().slice(0, 80);
        if (typeof entry.primaryQuota === 'string') setting.primaryQuota = entry.primaryQuota.slice(0, 128);
        if(['monochrome','color','inherit'].includes(entry.iconStyle))setting.iconStyle=entry.iconStyle;
        if(typeof entry.iconSource==='string'&&validId(entry.iconSource))setting.iconSource=entry.iconSource;
        if(typeof entry.customIcon==='string'&&entry.customIcon.length<=524288&&/^data:image\/(png|jpeg|webp);base64,[a-z0-9+/]+=*$/i.test(entry.customIcon))setting.customIcon=entry.customIcon;
        if(Array.isArray(entry.hiddenMetrics))setting.hiddenMetrics=[...new Set(entry.hiddenMetrics.filter(v=>typeof v==='string'&&v.length<=256))].slice(0,256);
        if(typeof entry.pinned==='boolean')setting.pinned=entry.pinned;
        result.providers[id] = setting;
      }
    }
    if (value.trends && typeof value.trends === 'object' && !Array.isArray(value.trends)) {
      result.trends = {};
      for (const key of ['provider','range','group','metric','breakdown','start','end']) {
        if (typeof value.trends[key] === 'string' && value.trends[key].length <= 128) result.trends[key] = value.trends[key];
      }
    }
    return result;
  }
  function provider(prefs, id) {
    return prefs.providers && Object.hasOwn(prefs.providers,id) ? prefs.providers[id] : {visible:true, name:'', primaryQuota:''};
  }
  function order(items, prefs, idOf) {
    const positions = new Map(prefs.providerOrder.map((id, index) => [id, index]));
    return items.slice().sort((a, b) => {
      const first=positions.get(idOf(a)) ?? Infinity, second=positions.get(idOf(b)) ?? Infinity;
      const pinA=provider(prefs,idOf(a)).pinned===true,pinB=provider(prefs,idOf(b)).pinned===true;
      return pinA!==pinB ? Number(pinB)-Number(pinA) : first===second ? 0 : first-second;
    });
  }
  function primary(snapshot, prefs) {
    const candidates = metrics(snapshot,prefs).filter(m => m.type === 'progress' && m.limit > 0);
    return candidates.find(m => m.label === provider(prefs, snapshot.providerId).primaryQuota) || candidates[0] || null;
  }
  function metricKey(metric){return metric.type+':'+metric.label;}
  function metrics(snapshot,prefs){const hidden=provider(prefs,snapshot.providerId).hiddenMetrics||[];return(snapshot.metrics||[]).filter(m=>!hidden.includes(metricKey(m)));}
  function thresholdAt(percent,prefs){return prefs.thresholds.filter(t=>percent>=t.percent).at(-1)||null;}
  function iconChoices(catalog,providerId){
    const id=catalog?.aliases?.[providerId]||providerId,icons=catalog?.icons||[];
    const own=icons.find(icon=>icon.id===id);if(!own)return [];
    const alternatives=new Set([id,...(own.alternatives||[])]);
    return [own,...icons.filter(icon=>icon.id!==id&&alternatives.has(icon.id))];
  }
  return {STORAGE_KEY, THEMES, ACCENTS, SORTS, REFRESH_SECONDS, COMPONENTS, normalize, provider, order, primary,metricKey,metrics,thresholdAt,iconChoices};
});
