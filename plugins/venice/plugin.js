(function () {
  var API_URL = "https://api.venice.ai/api/v1/billing/balance";

  function loadApiKey(ctx) {
    if (Object.prototype.hasOwnProperty.call(ctx.provider || {}, 'apiKey')) return ctx.provider.apiKey;
    if (ctx.provider && ctx.provider.instanceId && ctx.provider.instanceId !== 'venice') return null;
    var v = ctx.host.env.get("VENICE_API_KEY");
    if (typeof v === "string" && v.trim()) return v.trim();
    return null;
  }

  function probe(ctx) {
    if (ctx.sourceMode === 'web') return webUsage(ctx);
    var apiKey = loadApiKey(ctx);
    if (!apiKey) {
      throw "Venice API key not found. Set VENICE_API_KEY.";
    }

    var result = ctx.util.requestJson({
      method: "GET",
      url: API_URL,
      headers: { Authorization: "Bearer " + apiKey, Accept: "application/json" },
      timeoutMs: 15000,
    });

    if (ctx.util.isAuthStatus(result.resp.status)) {
      throw "API key invalid or expired.";
    }
    if (result.resp.status < 200 || result.resp.status >= 300) {
      throw "Venice API error (HTTP " + result.resp.status + ").";
    }
    if (!result.json) throw "Could not parse Venice balance response.";

    var json = result.json;
    var canConsume = json.canConsume !== false;
    var currency = (json.consumptionCurrency || "").toUpperCase();
    var balances = json.balances || {};
    var diem = typeof balances.diem === "number" ? balances.diem : null;
    var usd = typeof balances.usd === "number" ? balances.usd : null;
    var epochAlloc = typeof json.diemEpochAllocation === "number" ? json.diemEpochAllocation : null;

    var lines = [];

    if (!canConsume) {
      lines.push(ctx.line.badge({ label: "Balance", text: "Balance unavailable for API calls", color: "#ef4444" }));
      return { lines: lines };
    }

    if (currency === "USD" && usd !== null && usd > 0) {
      lines.push(ctx.line.text({ label: "Balance", value: "$" + usd.toFixed(2) + " USD" }));
    } else if (currency !== "USD" && diem !== null && epochAlloc !== null && epochAlloc > 0) {
      var usedPct = Math.max(0, Math.min(100, (epochAlloc - diem) / epochAlloc * 100));
      lines.push(ctx.line.progress({
        label: "DIEM",
        used: usedPct,
        limit: 100,
        format: { kind: "percent" },
      }));
      lines.push(ctx.line.text({ label: "Allocation", value: "DIEM " + diem.toFixed(2) + " / " + epochAlloc.toFixed(2) }));
    } else if (diem !== null && diem > 0) {
      lines.push(ctx.line.text({ label: "Balance", value: "DIEM " + diem.toFixed(2) }));
    } else if (usd !== null && usd > 0) {
      lines.push(ctx.line.text({ label: "Balance", value: "$" + usd.toFixed(2) + " USD" }));
    } else {
      lines.push(ctx.line.badge({ label: "Balance", text: "No Venice API balance", color: "#a3a3a3" }));
    }

    return { source: 'api', lines: lines };
  }

  function webUsage(ctx) {
    var provider = ctx.provider || {};
    if (provider.settings && provider.settings.cookies === 'off') throw new Error('Venice web cookies are disabled.');
    var raw = Object.prototype.hasOwnProperty.call(provider, 'cookieHeader') ? provider.cookieHeader :
      provider.instanceId && provider.instanceId !== 'venice' ? '' : ctx.host.env.get('VENICE_COOKIE');
    var cookies = {};
    String(raw || '').replace(/^Cookie:\s*/i, '').split(';').forEach(function(pair) {
      var at = pair.indexOf('='); if (at > 0) cookies[pair.slice(0,at).trim()] = pair.slice(at+1).trim();
    });
    var legacy = '__venice-auth.session-token';
    var token = cookies[legacy];
    var chunks = Object.keys(cookies).filter(function(key){return /^__venice-auth\.session-token\.\d+$/.test(key);});
    if (!token && chunks.length && chunks.every(function(_,i){return cookies[legacy+'.'+i];}))
      token = chunks.map(function(_,i){return cookies[legacy+'.'+i];}).join('');
    var headers = {Accept:'application/json'};
    if (token) headers.Cookie = legacy+'='+token;
    else {
      token = cookies.__session || cookies[Object.keys(cookies).sort().find(function(key){return /^__session_.+/.test(key);})];
      if (!token) throw new Error('Import a fresh Venice session cookie.');
      headers.Authorization = 'Bearer '+token;
    }
    var resp = ctx.host.http.request({method:'GET',url:'https://outerface.venice.ai/api/user/session',headers:headers,timeoutMs:15000});
    if (resp.status === 401 || resp.status === 403) throw new Error('Venice session expired. Reopen venice.ai and import fresh cookies.');
    if (resp.status < 200 || resp.status >= 300) throw new Error('Venice session request failed (HTTP '+resp.status+').');
    var payload = ctx.util.tryParseJson(resp.bodyText);
    var claims = payload && ctx.jwt.decodePayload(payload.token);
    function amount(value) { if (typeof value !== 'number' && typeof value !== 'string' || value === '') return null; var n=Number(value); return Number.isFinite(n)&&n>=0?n:null; }
    var expiry = claims && amount(claims.exp);
    if (!claims || expiry === null || expiry < 1e9 || expiry > 4e9 || expiry*1000 < Date.parse(ctx.nowIso)-60000)
      throw new Error('Venice session expired. Import fresh cookies.');
    if (['anonymous','anon','guest','unauthenticated','logged_out'].includes(String(claims.userType).toLowerCase())) throw new Error('Sign in to Venice.');
    var usage = claims.bundledCreditsUsage || {};
    var used = amount(usage.usedThisCycle), refill = amount(usage.monthlyRefillCredits);
    if (used === null || !(refill > 0)) throw new Error('Venice subscription credits unavailable.');
    var lines = [];
    var available = amount(usage.availableCredits);
    if (available === null) available = amount(claims.bundledCredits);
    [['Subscription credits available',available],['Total credits available',amount(claims.veniceCredits)],['Bank cap',amount(usage.tierCap)]]
      .forEach(function(pair){if(pair[1] !== null) lines.push(ctx.line.text({label:pair[0],value:String(pair[1])}));});
    lines.push(ctx.line.text({label:'Used this cycle',value:String(used),subtitle:'Monthly refill: '+refill}));
    var next = amount(usage.nextRefillAt);
    if (next >= 1e12 && next <= 4e12) lines.push(ctx.line.text({label:'Next refill',value:new Date(next).toISOString()}));
    // A refill is not a spending cap. Banked credits must never appear exhausted.
    return {source:'web',lines:lines};
  }

  globalThis.__openusage_plugin = { id: "venice", probe: probe };
})();
