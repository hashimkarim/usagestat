(function () {
  const PLATFORM = 'https://platform.deepseek.com/api/v0/';
  const own = (value, key) => Object.prototype.hasOwnProperty.call(value, key);
  const clean = value => typeof value === 'string' && value.trim() ? value.trim().replace(/^Bearer\s+/i, '') : null;
  function amount(value) {
    if (typeof value !== 'number' && typeof value !== 'string' || typeof value === 'string' && !value.trim()) return null;
    const n = Number(value);
    return Number.isFinite(n) && n >= 0 ? n : null;
  }
  function fail(code, message) { throw {code, message}; }
  function request(ctx, url, token, timeoutMs) {
    const result = ctx.util.requestJson({method:'GET', url, timeoutMs,
      headers:{Authorization:'Bearer '+token, Accept:'application/json', 'x-client-platform':'web'}});
    const body = result.json;
    if ([401,403].includes(result.resp.status) || [40002,40003].includes(body?.code) || [40002,40003].includes(body?.data?.biz_code))
      fail('credential-rejected', 'DeepSeek credential expired. Sign in again.');
    if (result.resp.status === 429) fail('failed', 'DeepSeek is rate limited.');
    if (result.resp.status < 200 || result.resp.status >= 300 || !body || body.code && body.code !== 0 || body.data?.biz_code && body.data.biz_code !== 0)
      fail('failed', 'DeepSeek request failed (HTTP '+result.resp.status+').');
    return body;
  }
  function choose(values) {
    return values.find(v => v.currency === 'USD' && v.total > 0) || values.find(v => v.total > 0) || values.find(v => v.currency === 'USD') || values[0];
  }
  function balance(ctx, key, token) {
    if (key) {
      const data = request(ctx, 'https://api.deepseek.com/user/balance', key, 15000);
      if (!Array.isArray(data.balance_infos)) fail('failed', 'DeepSeek balance response changed.');
      const values = data.balance_infos.map(value => ({currency:String(value.currency || '').toUpperCase(), total:amount(value.total_balance),
        paid:amount(value.topped_up_balance), granted:amount(value.granted_balance)}));
      if (values.some(v => !v.currency || v.total === null || v.paid === null || v.granted === null)) fail('failed', 'DeepSeek returned an invalid balance.');
      return {value:choose(values), available:data.is_available !== false};
    }
    const data = request(ctx, PLATFORM+'users/get_user_summary', token, 15000).data?.biz_data;
    if (!data || !Array.isArray(data.normal_wallets) || !Array.isArray(data.bonus_wallets)) fail('failed', 'DeepSeek wallet response changed.');
    const values = new Map();
    for (const [field, wallets] of [['paid',data.normal_wallets],['granted',data.bonus_wallets]]) {
      for (const wallet of wallets) {
        const currency = String(wallet.currency || '').toUpperCase(), value = amount(wallet.balance);
        if (!currency || value === null) fail('failed', 'DeepSeek returned an invalid wallet.');
        if (!values.has(currency)) values.set(currency,{currency,total:0,paid:0,granted:0});
        values.get(currency)[field] += value;
        values.get(currency).total += value;
      }
    }
    return {value:choose([...values.values()]),available:true};
  }
  function details(ctx, token, lines) {
    const now = new Date(ctx.nowIso), end = Date.parse(ctx.nowIso.slice(0,10)) + 86400000, start = end - 30*86400000;
    const deadline = Date.now() + 5000;
    const fetch = suffix => {
      const left = deadline - Date.now();
      if (left <= 0) fail('failed','DeepSeek detail deadline exceeded.');
      return request(ctx, PLATFORM+'usage/'+suffix, token, Math.min(1500,left)).data?.biz_data;
    };
    let amounts, costs, monthly = false;
    try {
      const query = '?start='+Math.floor(start/1000)+'&end='+Math.floor(end/1000)+'&tz=0';
      amounts = fetch('by_api_key/amount'+query);
      costs = fetch('by_api_key/cost'+query);
      if (!Array.isArray(amounts?.series) || !Array.isArray(costs?.data)) throw new Error('Unrecognized series');
    } catch (error) {
      if (error?.code === 'credential-rejected' || /rate limited/.test(error?.message || '')) throw error;
      monthly = true;
      const query = '?month='+(now.getUTCMonth()+1)+'&year='+now.getUTCFullYear();
      amounts = fetch('amount'+query);
      costs = fetch('cost'+query);
      if (!Array.isArray(amounts?.days) || !Array.isArray(costs)) throw new Error('Unrecognized monthly usage');
    }
    const blocks = monthly ? costs : costs.data;
    const block = blocks.find(b => b.currency === 'USD') || blocks[0];
    if (!block || typeof block.currency !== 'string') throw new Error('Unknown currency');
    const currency = block.currency.toUpperCase(), days = new Map(), models = new Map(), keys = new Set();
    const lower = monthly ? ctx.nowIso.slice(0,7)+'-01' : new Date(start).toISOString().slice(0,10), upper = ctx.nowIso.slice(0,10);
    function day(date) {
      if (!/^\d{4}-\d{2}-\d{2}$/.test(date) || date < lower || date > upper || new Date(date).toISOString().slice(0,10) !== date) return null;
      if (!days.has(date)) days.set(date,{date,inputTokens:0,outputTokens:0,cacheReadTokens:0,totalTokens:0,costUsd:0,
        requests:0,tokensKnown:false,costKnown:false,amountSeen:false,costSeen:false,badTokens:false,badCost:false});
      return days.get(date);
    }
    function addAmount(date, usage) {
      const row = day(date);
      if (!row || !usage || typeof usage !== 'object') return;
      for (const [type, field] of [['PROMPT_CACHE_MISS_TOKEN','inputTokens'],['PROMPT_CACHE_HIT_TOKEN','cacheReadTokens'],['RESPONSE_TOKEN','outputTokens'],['REQUEST','requests']]) {
        if (!own(usage,type)) continue;
        const value = amount(usage[type]);
        if (value === null || !Number.isSafeInteger(value)) { row.badTokens=true; continue; }
        row[field] += value;
        if (field !== 'requests') { row.totalTokens += value; row.amountSeen=true; }
      }
    }
    function addCost(date, model, raw) {
      const row = day(date), value = amount(raw);
      if (!row) return;
      if (value === null) { row.badCost=true; if(model) models.set(model,null); return; }
      row.costUsd += value; row.costSeen=true;
      if (model && models.get(model) !== null) models.set(model,(models.get(model)||0)+value);
    }
    const dateFor = time => typeof time === 'number' && time*1000 >= start && time*1000 < end ? new Date(time*1000).toISOString().slice(0,10) : '';
    if (!monthly) {
      if (amounts.series.length > 10000 || !Array.isArray(block.series) || block.series.length > 10000) throw new Error('Too many series');
      for (const series of amounts.series) {
        if (series.api_key) keys.add(JSON.stringify(series.api_key));
        if (!Array.isArray(series.buckets) || series.buckets.length > 32) throw new Error('Invalid buckets');
        for (const bucket of series.buckets) addAmount(dateFor(bucket.time),bucket.usage);
      }
      for (const series of block.series) {
        if (series.api_key) keys.add(JSON.stringify(series.api_key));
        if (!Array.isArray(series.buckets) || series.buckets.length > 32) throw new Error('Invalid buckets');
        for (const bucket of series.buckets) addCost(dateFor(bucket.time),series.model,bucket.cost);
      }
    } else {
      for (const item of amounts.days.slice(0,32)) for (const model of (item.data || []).slice(0,1000))
        addAmount(item.date, Object.fromEntries((model.usage || []).map(value => [value.type,value.amount])));
      for (const item of (block.days || []).slice(0,32)) for (const model of (item.data || []).slice(0,1000))
        for (const value of model.usage || []) addCost(item.date,model.model,value.amount);
    }
    const daily = [...days.values()].sort((a,b)=>a.date.localeCompare(b.date));
    for (const row of daily) {
      row.tokensKnown = row.amountSeen && !row.badTokens && Number.isSafeInteger(row.totalTokens);
      row.costKnown = row.costSeen && !row.badCost && Number.isFinite(row.costUsd);
      if (!Number.isFinite(row.costUsd)) throw new Error('Cost overflow');
      delete row.amountSeen; delete row.costSeen; delete row.badTokens; delete row.badCost;
    }
    if (!daily.length) return;
    const total = daily.reduce((sum,row)=>sum+row.costUsd,0), complete = daily.every(row=>row.costKnown);
    if(!Number.isFinite(total)) throw new Error('Cost overflow');
    const usageLines = [
      ctx.line.text({label:monthly?'This month':'Last 30 days',value:currency+' '+total.toFixed(2)+(complete?'':'+'),subtitle:'Platform-account spend across API keys'}),
      ctx.line.text({label:'Tokens',value:String(daily.reduce((sum,row)=>sum+row.totalTokens,0))+(daily.every(row=>row.tokensKnown)?'':'+')}),
      ctx.line.text({label:'Requests',value:String(daily.reduce((sum,row)=>sum+row.requests,0))})
    ];
    if(keys.size) usageLines.push(ctx.line.text({label:'API keys',value:String(keys.size)}));
    for(const [model,cost] of [...models].filter(([,v])=>v!==null && Number.isFinite(v)).sort((a,b)=>b[1]-a[1]).slice(0,30))
      usageLines.push(ctx.line.text({label:model,value:currency+' '+cost.toFixed(2)}));
    usageLines.push(ctx.line.barChart({label:'Daily tokens',points:daily.map(row=>({label:row.date,value:row.totalTokens}))}));
    usageLines.push(ctx.line.barChart({label:'Daily spend',points:daily.filter(row=>row.costKnown).map(row=>({label:row.date,value:row.costUsd,valueLabel:currency+' '+row.costUsd.toFixed(2)}))}));
    // The shared ledger is USD-only; never relabel CNY spend as dollars.
    const stored = daily.map(row=>currency==='USD'?row:{...row,costUsd:0,costKnown:false});
    ctx.host.usageDaily.ingest({displayName:'DeepSeek',source:'deepseek_platform',daily:stored});
    lines.push(...usageLines);
  }
  function probe(ctx) {
    const provider = ctx.provider || {}, settings = provider.settings || {}, scoped = provider.instanceId && provider.instanceId !== 'deepseek';
    const env = key => scoped ? null : clean(ctx.host.env.get(key));
    const key = ctx.sourceMode === 'web' ? null : own(provider,'apiKey') ? clean(provider.apiKey) : env('DEEPSEEK_API_KEY') || env('DEEPSEEK_KEY');
    const token = own(settings,'platformToken') ? clean(settings.platformToken) : own(provider,'cookieHeader') ? clean(provider.cookieHeader) : env('DEEPSEEK_PLATFORM_TOKEN') || env('DEEPSEEK_USER_TOKEN');
    if (!key && !token || ctx.sourceMode === 'api' && !key) fail('missing-auth','Set a DeepSeek API key or a Platform token in web mode.');
    const data = balance(ctx,key,token), lines = [], value = data.value;
    if (value) lines.push(ctx.line.text({label:'Balance',value:value.currency+' '+value.total.toFixed(2),
      subtitle:'Paid: '+value.paid.toFixed(2)+' / Granted: '+value.granted.toFixed(2)}));
    else lines.push(ctx.line.badge({label:'Balance',text:'No balance data',color:'#a3a3a3'}));
    if (!data.available) lines.push(ctx.line.badge({label:'Status',text:'Balance unavailable for API calls',color:'#ef4444'}));
    // Pair website totals only with an explicitly bound API key, never ambient account state.
    if (token && (!key || own(settings,'platformToken') && settings.platformApiKeyHash === ctx.host.crypto.sha256(key)) && settings.extraUsage !== false) {
      try { details(ctx,token,lines); }
      catch (_) { lines.push(ctx.line.text({label:'Details',value:'Detailed usage unavailable'})); }
    }
    return {source:key?'api':'web',lines};
  }
  globalThis.__openusage_plugin = {id:'deepseek',probe};
})();
