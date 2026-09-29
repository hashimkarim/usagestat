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
  const validId = id => typeof id === 'string' && /^[a-zA-Z0-9][a-zA-Z0-9_.:-]{0,127}$/.test(id) && !['constructor','prototype'].includes(id);
  function normalize(value) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid preferences.');
    const result = {
      search:'', status:'all', sort:'order', quotaDisplay:'used', inactive:'hide',
      theme:'dark', accent:'blue', density:'comfortable', icons:true,
      summary:true, refreshSeconds:30, providerOrder:[], providers:{},
    };
    if (typeof value.search === 'string') result.search = value.search.slice(0, 256);
    if (['all','live','attention','near','tracked'].includes(value.status)) result.status = value.status;
    if (SORTS.includes(value.sort)) result.sort = value.sort;
    if (value.quotaDisplay === 'remaining') result.quotaDisplay = 'remaining';
    if (value.inactive === 'show') result.inactive = 'show';
    if (THEMES.includes(value.theme)) result.theme = value.theme;
    if (Object.hasOwn(ACCENTS, value.accent)) result.accent = value.accent;
    if (value.density === 'compact') result.density = 'compact';
    for (const key of ['icons','summary']) if (typeof value[key] === 'boolean') result[key] = value[key];
    if (REFRESH_SECONDS.includes(value.refreshSeconds)) result.refreshSeconds = value.refreshSeconds;
    if (Array.isArray(value.providerOrder)) result.providerOrder = [...new Set(value.providerOrder.filter(validId))].slice(0, 256);
    if (value.providers && typeof value.providers === 'object' && !Array.isArray(value.providers)) {
      for (const [id, entry] of Object.entries(value.providers).slice(0, 256)) {
        if (!validId(id) || !entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
        const setting = {visible:true, name:'', primaryQuota:''};
        if (typeof entry.visible === 'boolean') setting.visible = entry.visible;
        if (typeof entry.name === 'string') setting.name = entry.name.trim().slice(0, 80);
        if (typeof entry.primaryQuota === 'string') setting.primaryQuota = entry.primaryQuota.slice(0, 128);
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
      return first===second ? 0 : first-second;
    });
  }
  function primary(snapshot, prefs) {
    const candidates = (snapshot.metrics || []).filter(m => m.type === 'progress' && m.format?.kind === 'percent' && m.limit > 0);
    return candidates.find(m => m.label === provider(prefs, snapshot.providerId).primaryQuota) || candidates[0] || null;
  }
  return {STORAGE_KEY, THEMES, ACCENTS, SORTS, REFRESH_SECONDS, normalize, provider, order, primary};
});
