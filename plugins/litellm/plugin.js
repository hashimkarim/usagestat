(function () {
  function env(ctx, name) {
    try {
      var value = ctx.host.env.get(name);
      return typeof value === "string" && value.trim() ? value.trim() : null;
    } catch (_) {
      return null;
    }
  }

  function apiKey(ctx) {
    if (Object.prototype.hasOwnProperty.call(ctx.provider || {}, 'apiKey')) return ctx.provider.apiKey;
    if (ctx.provider && ctx.provider.instanceId && ctx.provider.instanceId !== 'litellm') return null;
    var configured = ctx.provider && typeof ctx.provider.apiKey === "string" ? ctx.provider.apiKey.trim() : "";
    return configured || env(ctx, "LITELLM_API_KEY");
  }

  function setting(ctx, names) {
    var settings = ctx.provider && ctx.provider.settings ? ctx.provider.settings : {};
    for (var i = 0; i < names.length; i++) {
      var value = settings[names[i]];
      if (typeof value === "string" && value.trim()) return value.trim();
    }
    return null;
  }

  function baseUrl(ctx) {
    var configured = setting(ctx, ["enterpriseHost", "baseUrl", "baseURL", "apiUrl", "apiURL"]);
    var base = configured || env(ctx, "LITELLM_BASE_URL");
    if (!base) throw "Missing LiteLLM base URL. Set LITELLM_BASE_URL or provider settings.enterpriseHost.";
    base = base.trim().replace(/\/+$/, "");
    if (base.toLowerCase().endsWith("/v1")) base = base.slice(0, -3).replace(/\/+$/, "");
    if (!/^https?:\/\//i.test(base)) throw "LiteLLM base URL is invalid.";
    return base;
  }

  function numberValue(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.replace(/[$,]/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function nonEmpty(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null;
  }

  function parseDate(value) {
    if (typeof value !== "string" || !value.trim()) return null;
    var ms = Date.parse(value);
    return Number.isFinite(ms) ? new Date(ms) : null;
  }

  function requestJson(ctx, base, path, key, optional) {
    var resp = ctx.util.request({
      method: "GET",
      url: base + path,
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    if (optional && [401,403,404].includes(resp.status)) return null;
    if (ctx.util.isAuthStatus(resp.status)) throw "LiteLLM API key was rejected.";
    if (resp.status < 200 || resp.status >= 300) {
      throw "LiteLLM API request failed (HTTP " + resp.status + ").";
    }
    var json = ctx.util.tryParseJson(resp.bodyText);
    if (!json) throw "LiteLLM response was not valid JSON.";
    return json;
  }

  function keyInfo(json) {
    var info = json && json.info ? json.info : json;
    var userID = nonEmpty(info && (info.user_id || info.userID));
    var teamID = nonEmpty(info && (info.team_id || info.teamID));
    if (!userID && !teamID) throw "LiteLLM key info did not include a user_id or team_id.";
    return {
      userID: userID,
      teamID: teamID,
      keyName: nonEmpty(info.key_name || info.keyName),
      spendUSD: numberValue(info.spend) || 0,
      expiresAt: parseDate(info.expires),
    };
  }

  function findTeam(teams, teamID) {
    if (!Array.isArray(teams) || !teamID) return null;
    for (var i = 0; i < teams.length; i++) {
      if (String(teams[i].team_id || teams[i].teamID || "") === teamID) return teams[i];
    }
    return null;
  }

  function usd(value) {
    return "$" + (Number(value) || 0).toFixed(2);
  }

  function spendDetail(spend, budget, prefix) {
    var value = budget != null && budget > 0 ? usd(spend) + " / " + usd(budget) : usd(spend);
    return prefix ? prefix + ": " + value : value;
  }

  function spendLabel(label) {
    return label.replace(/\s+budget$/i, " spend");
  }

  function addBudget(lines, label, spend, budget, resetAt, detailPrefix) {
    if (budget != null && budget > 0) {
      var line = ctxLineProgress(label, (spend / budget) * 100);
      if (resetAt) line.resetsAt = resetAt.toISOString();
      line.detail = spendDetail(spend, budget, detailPrefix);
      lines.push(line);
      return true;
    } else if (spend > 0) {
      lines.push({ type: "text", label: spendLabel(label), value: spendDetail(spend, null, detailPrefix) });
      return true;
    }
    return false;
  }

  function ctxLineProgress(label, percent) {
    return {
      type: "progress",
      label: label,
      used: Math.max(0, Math.min(100, percent)),
      limit: 100,
      format: { kind: "percent" },
    };
  }

  function userSnapshot(ctx, base, key, info) {
    var encoded = encodeURIComponent(info.userID);
    var json = requestJson(ctx, base, "/user/info?user_id=" + encoded, key);
    var user = json.user_info || json.userInfo || json;
    var responseID = nonEmpty(user.user_id || user.userID || json.user_id || json.userID);
    if (responseID && responseID !== info.userID) throw "LiteLLM user_id did not match /key/info.";
    var team = findTeam(json.teams, info.teamID);
    return {
      userID: info.userID,
      email: nonEmpty(user.user_email || user.userEmail || user.user_alias || user.userAlias || (user.metadata && user.metadata.preferred_username)),
      personalSpendUSD: numberValue(user.spend) || 0,
      personalBudgetUSD: numberValue(user.max_budget || user.maxBudget),
      personalResetAt: parseDate(user.budget_reset_at || user.budgetResetAt),
      team: team ? {
        id: String(team.team_id || team.teamID),
        alias: nonEmpty(team.team_alias || team.teamAlias),
        spendUSD: numberValue(team.spend) || 0,
        budgetUSD: numberValue(team.max_budget || team.maxBudget),
        resetAt: parseDate(team.budget_reset_at || team.budgetResetAt),
      } : null,
      keyName: info.keyName,
      keyExpiresAt: info.expiresAt,
    };
  }

  function teamSnapshot(ctx, base, key, info) {
    var encoded = encodeURIComponent(info.teamID);
    var json = requestJson(ctx, base, "/team/info?team_id=" + encoded, key);
    var team = json.team_info || json.teamInfo || json;
    var responseID = nonEmpty(team.team_id || team.teamID || json.team_id || json.teamID);
    if (responseID && responseID !== info.teamID) throw "LiteLLM team_id did not match /key/info.";
    return {
      userID: null,
      email: null,
      personalSpendUSD: 0,
      personalBudgetUSD: null,
      personalResetAt: null,
      team: {
        id: info.teamID,
        alias: nonEmpty(team.team_alias || team.teamAlias),
        spendUSD: numberValue(team.spend) || 0,
        budgetUSD: numberValue(team.max_budget || team.maxBudget),
        resetAt: parseDate(team.budget_reset_at || team.budgetResetAt),
      },
      keyName: info.keyName,
      keyExpiresAt: info.expiresAt,
    };
  }

  function linesFor(snapshot) {
    var lines = [];
    addBudget(lines, "Personal budget", snapshot.personalSpendUSD, snapshot.personalBudgetUSD, snapshot.personalResetAt);
    if (snapshot.team) addBudget(lines, "Team budget", snapshot.team.spendUSD, snapshot.team.budgetUSD, snapshot.team.resetAt, snapshot.team.alias ? "Team " + snapshot.team.alias : "Team");
    if (!lines.length) lines.push({ type: "text", label: "Spend", value: usd(snapshot.personalSpendUSD || (snapshot.team && snapshot.team.spendUSD) || 0) });
    if (snapshot.keyName) lines.push({ type: "text", label: "Key", value: snapshot.keyName });
    if (snapshot.keyExpiresAt) lines.push({ type: "text", label: "Key Expires", value: snapshot.keyExpiresAt.toISOString().slice(0, 10) });
    return lines;
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "Missing LiteLLM API key. Set LITELLM_API_KEY or provider apiKey.";
    var base = baseUrl(ctx);
    var keyData = requestJson(ctx, base, "/key/info", key, true);
    if (keyData === null) {
      var end = ctx.nowIso.slice(0,10), start = end.slice(0,7)+'-01';
      var query = '?start_date='+start+'&end_date='+end;
      var rows = requestJson(ctx, base, '/key/spend/report'+query, key, true);
      if (rows === null) rows = requestJson(ctx, base, '/user/spend/report'+query, key);
      if (!Array.isArray(rows) || !rows.length) throw new Error('LiteLLM spend report is empty or invalid.');
      var cost = 0;
      for (var row of rows) {
        if (typeof row.total_cost !== 'number' || !Number.isFinite(row.total_cost) || row.total_cost < 0) throw new Error('Invalid LiteLLM spend.');
        cost += row.total_cost;
      }
      if (!Number.isFinite(cost)) throw new Error('Invalid LiteLLM spend total.');
      return {source:'api',lines:[ctx.line.text({label:'Month to date',value:usd(cost),subtitle:start+' to '+end+' UTC'})]};
    }
    var info = keyInfo(keyData);
    var snapshot = info.userID ? userSnapshot(ctx, base, key, info) : teamSnapshot(ctx, base, key, info);
    var plan = snapshot.team && snapshot.team.alias ? snapshot.team.alias : null;
    var lines = linesFor(snapshot);
    if (info.userID && (ctx.provider?.settings?.modelUsage === true || ctx.provider?.settings?.LITELLM_MODEL_USAGE_ENABLED === 'true')) {
      try { lines.push.apply(lines, modelActivity(ctx,base,key,info.userID)); } catch (_) { ctx.host.log.warn('LiteLLM optional model activity unavailable.'); }
    }
    return { displayName: "LiteLLM", source: "api", plan: plan, lines: lines };
  }

  function modelActivity(ctx,base,key,user) {
    var end=ctx.nowIso.slice(0,10),start=new Date(Date.parse(end)-29*86400000).toISOString().slice(0,10),totals={};
    for(var page=1;page<=3;page++) {
      var response=ctx.host.http.request({method:'GET',url:base+'/user/daily/activity?user_id='+encodeURIComponent(user)+'&start_date='+start+'&end_date='+end+'&page='+page+'&page_size=1000',
        headers:{Authorization:'Bearer '+key,Accept:'application/json'},timeoutMs:2000});
      if(response.status!==200) throw new Error('Unavailable');
      var data=ctx.util.tryParseJson(response.bodyText);
      if(!Array.isArray(data?.results)||data.results.length>31) throw new Error('Invalid activity');
      for(var row of data.results) {
        if(!/^\d{4}-\d{2}-\d{2}$/.test(row.date)||row.date<start||row.date>end||!row.breakdown?.models) throw new Error('Invalid day');
        for(var [name, value] of Object.entries(row.breakdown.models)) {
          if(!name.trim()||name.length>256||/[\x00-\x1f]/.test(name)||['__proto__','constructor','prototype'].includes(name)) throw new Error('Invalid model');
          var metrics=value.metrics||value,prior=totals[name]||[0,0,0,0];
          totals[name]=['prompt_tokens','completion_tokens','total_tokens','api_requests'].map(function(field,i){
            var n=metrics[field]; if(!Number.isSafeInteger(n)||n<0||!Number.isSafeInteger(n+prior[i])) throw new Error('Invalid count'); return n+prior[i];
          });
          if(Object.keys(totals).length>1000) throw new Error('Too many models');
        }
      }
      var metadata=data.metadata||{};
      if(metadata.page!=null&&metadata.page!==page) throw new Error('Repeated page');
      if(metadata.has_more!=null&&typeof metadata.has_more!=='boolean') throw new Error('Invalid page');
      if(metadata.total_pages!=null&&(!Number.isSafeInteger(metadata.total_pages)||metadata.total_pages<1)) throw new Error('Invalid page');
      if(metadata.has_more!==true&&page>=(metadata.total_pages||1)) return Object.entries(totals).sort((a,b)=>b[1][2]-a[1][2]).slice(0,20)
        .map(([name,n])=>ctx.line.text({label:name,value:n[2]+' tokens / '+n[3]+' requests',subtitle:'30d UTC: '+n[0]+' input / '+n[1]+' output'}));
    }
    throw new Error('Incomplete activity');
  }

  globalThis.__openusage_plugin = { id: "litellm", probe: probe };
})();
