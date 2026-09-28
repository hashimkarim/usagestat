(function () {
  const own = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
  const validCount = n => Number.isSafeInteger(n) && n >= 0;
  const clean = s => typeof s === 'string' && s.trim() ? s.trim() : null;
  const DAY = 86400000;
  function roots(ctx) {
    const settings = ctx.provider?.settings || {};
    if (own(settings, 'sessionRoots')) {
      if (!Array.isArray(settings.sessionRoots) || settings.sessionRoots.some(p => !clean(p))) throw new Error('Invalid Pi sessionRoots.');
      return [...new Set(settings.sessionRoots)];
    }
    if (ctx.provider?.instanceId && ctx.provider.instanceId !== 'pi') throw new Error('Set sessionRoots for this Pi account.');
    const env = k => clean(ctx.host.env.get(k));
    const sessionDir = env('PI_CODING_AGENT_SESSION_DIR');
    if (sessionDir) return [sessionDir];
    const agentDir = env('PI_CODING_AGENT_DIR');
    if (agentDir) return [agentDir.replace(/[\\/]+$/, '') + '/sessions'];
    const home = ctx.host.fs.homeDir;
    const config = env('PI_CONFIG_DIR') || '.omp';
    if (/^(?:[\\/]|[A-Za-z]:)/.test(config) || config.split(/[\\/]/).includes('..')) throw new Error('PI_CONFIG_DIR must be inside the home directory.');
    const xdg = env('XDG_DATA_HOME') || home + '/.local/share';
    const profile = env('OMP_PROFILE') || env('PI_PROFILE');
    if (profile && profile !== 'default') {
      if (!/^[A-Za-z0-9_-]+$/.test(profile)) throw new Error('Invalid OMP profile.');
      return [home + '/' + config, xdg + '/omp'].flatMap(root =>
        [root + '/profiles/' + profile + '/sessions', root + '/profiles/' + profile + '/agent/sessions']);
    }
    return [home + '/.pi/agent/sessions', home + '/' + config + '/agent/sessions', xdg + '/omp/sessions'];
  }
  // API-equivalent USD estimates, reviewed against CodexBar 78ba5a1 (not subscription charges).
  function cost(message, counts, date) {
    const model = String(message.model || '').replace(/^(openai|anthropic)\//, '');
    const [input, output, read, write] = counts;
    let rate;
    if (message.provider === 'openai-codex') {
      if (model === 'gpt-5.4') rate = input + read > 272000 ? [5,22.5,.5,5] : [2.5,15,.25,2.5];
      else if (model === 'gpt-5.4-mini') rate = [.75,4.5,.075,.75];
      else if (model === 'gpt-5.4-nano') rate = [.2,1.25,.02,.2];
    } else if (message.provider === 'anthropic') {
      if (/^claude-sonnet-4-6(?:-\d{8})?$/.test(model)) rate = [3,15,.3,3.75];
      if (/^claude-opus-4-[67](?:-\d{8})?$/.test(model)) rate = [5,25,.5,6.25];
      // Earlier long-context pricing differs; keep it unpriced until its historical rate is available.
      if (date < '2026-03-13' && input + read + write > 200000) return null;
    }
    if (!rate) return null;
    const value = counts.reduce((sum, n, i) => sum + n * rate[i] / 1e6, 0);
    return Number.isFinite(value) ? value : null;
  }
  function probe(ctx) {
    const daily = new Map(), seen = new Set(), paths = new Set();
    const now = Date.parse(ctx.nowIso), start = Date.now();
    let bytes = 0, files = 0, complete = true;
    function visit(root, depth) {
      if (paths.has(root)) return;
      paths.add(root);
      if (depth > 6 || paths.size > 20000 || bytes > 128 * 1024 * 1024 || Date.now() - start > 10000) { complete = false; return; }
      if (!root.endsWith('.jsonl')) {
        let entries;
        try { entries = ctx.host.fs.listDir(root); } catch (_) { if (depth || ctx.host.fs.exists(root)) complete = false; return; }
        for (const name of entries) if (name !== '.' && name !== '..' && !/[\\/]/.test(name)) visit(root + '/' + name, depth + 1);
        return;
      }
      let content;
      try { content = ctx.host.fs.readTextLimited(root, 8 * 1024 * 1024); } catch (_) { complete = false; return; }
      bytes += content.length; files++;
      let session = root;
      for (const line of content.split('\n')) {
        if (!line.trim()) continue;
        let entry;
        try { entry = JSON.parse(line); } catch (_) { complete = false; continue; }
        if (entry.type === 'session' && clean(entry.id)) session = entry.id;
        const m = entry.message;
        if (entry.type !== 'message' || m?.role !== 'assistant') continue;
        const stamp = typeof m.timestamp === 'number' ? m.timestamp : Date.parse(entry.timestamp);
        if (!Number.isFinite(stamp)) { complete = false; continue; }
        if (stamp > now) continue;
        const id = session + ':' + (entry.id || ctx.host.crypto.sha256(line));
        if (seen.has(id)) continue;
        seen.add(id);
        const date = new Date(stamp).toISOString().slice(0, 10);
        if (!daily.has(date)) daily.set(date, {date, inputTokens:0, outputTokens:0, cacheReadTokens:0,
          cacheCreationTokens:0, totalTokens:0, costUsd:0, tokensKnown:true, costKnown:true, requests:0});
        const row = daily.get(date), u = m.usage;
        row.requests++;
        const counts = [u?.input, u?.output, u?.cacheRead ?? 0, u?.cacheWrite ?? 0];
        if (!u || !counts.every(validCount)) { row.tokensKnown = false; row.costKnown = false; continue; }
        const total = counts.reduce((a, b) => a + b, 0);
        if (!validCount(row.totalTokens + total)) { row.tokensKnown = false; row.costKnown = false; continue; }
        ['inputTokens','outputTokens','cacheReadTokens','cacheCreationTokens'].forEach((key, i) => row[key] += counts[i]);
        row.totalTokens += total;
        const amount = cost(m, counts, date);
        if (amount === null) row.costKnown = false; else row.costUsd += amount;
      }
    }
    for (const root of roots(ctx)) visit(root.replace(/[\\/]+$/, ''), 0);
    const rows = [...daily.values()].sort((a,b)=>a.date.localeCompare(b.date));
    if (!complete) rows.forEach(row=>{row.tokensKnown=false;row.costKnown=false;});
    if (rows.length) ctx.host.usageDaily.ingest({displayName:'Pi / OMP', source:'pi_transcript_estimated', daily:rows});
    const lines = [];
    for (const [label, days] of [['Today',1],['Last 30 Days',30]]) {
      const since = new Date(Date.parse(ctx.nowIso.slice(0,10)) - (days-1)*DAY).toISOString().slice(0,10);
      const selected = rows.filter(row=>row.date >= since);
      if (!selected.length) continue;
      const tokens = selected.reduce((sum,r)=>sum+r.totalTokens,0), usd=selected.reduce((sum,r)=>sum+r.costUsd,0);
      const priced = selected.every(r=>r.costKnown), counted = selected.every(r=>r.tokensKnown);
      lines.push(ctx.line.text({label, value:`${tokens}${counted?'':'+'} tokens`,
        subtitle:priced ? '$'+usd.toFixed(2)+' estimated API cost' : usd>0 ? '$'+usd.toFixed(2)+'+ partial estimated API cost' : 'Pricing unavailable'}));
    }
    if (!complete) lines.push(ctx.line.text({label:'History',value:'Partial scan'}));
    return {displayName:'Pi / OMP',source:'local',state:rows.length?'ready':'no-data',lines};
  }
  globalThis.__usagestat_plugin = {id:'pi',probe};
})();
