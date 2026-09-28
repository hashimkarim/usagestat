// Adapter for the reviewed CodexBar provider definitions bundled with usagestat.
(function () {
  const finite = value => typeof value === 'number' && Number.isFinite(value);
  const clean = value => typeof value === 'string' && value.trim() ? value.trim() : null;
  const own = (value, key) => Object.prototype.hasOwnProperty.call(value, key);
  function failure(code, message) {
    const state = code === 'authRequired' ? 'missing-auth' : code === 'error' || code === 'rateLimited' ? 'failed' : code;
    return Object.assign(new Error(String(message)), {code: state});
  }
  function iso(value) {
    const date = new Date(value);
    if (!Number.isFinite(date.getTime())) throw failure('error', 'Invalid provider date.');
    return date;
  }
  function number(value, options = {}) {
    if (!finite(value)) throw failure('error', 'Invalid provider amount.');
    // QuickJS has no Intl. Keep formatting deterministic on every native target.
    const min = Math.max(0, Math.min(8, options.minimumFractionDigits || 0));
    const max = Math.max(min, Math.min(8, options.maximumFractionDigits ?? 2));
    const parts = value.toFixed(max).split('.');
    while (parts[1]?.endsWith('0') && parts[1].length > min) parts[1] = parts[1].slice(0, -1);
    return parts[0].replace(/\B(?=(\d{3})+(?!\d))/g, ',') + (parts[1] ? '.' + parts[1] : '');
  }
  function readJSON(ctx, path) {
    try { return JSON.parse(ctx.host.fs.readText(path)); } catch (_) { return null; }
  }
  function context(ctx, definition, options) {
    const provider = ctx.provider || {};
    const settings = provider.settings || {};
    const scoped = !!provider.instanceId && provider.instanceId !== definition.id;
    const env = key => scoped ? null : clean(ctx.host.env.get(key));
    const secretKeys = definition.settings.filter(s => s.type === 'secure').map(s => s.key);
    function get(key) {
      if (own(settings, key)) {
        const value = String(settings[key]);
        return key === 'LLMMAN_HOST' ? value.replace(/\/+$/, '').replace(/\/v1$/, '') : value;
      }
      if (key === 'SOURCE_MODE') return ctx.sourceMode;
      if (key === 'V0_SCOPE' || key === 'GITKRAKEN_ORG_ID' || key === 'MUSE_WEB_TEAM_ID')
        return clean(provider.workspaceId) || env(key);
      if (/_BASE_URL$/.test(key) || key === 'LLMMAN_HOST' || key === 'PORTAL_URL') {
        const value = clean(settings.baseUrl) || clean(settings.baseURL) || env(key) || options.defaults?.[key] || null;
        return key === 'LLMMAN_HOST' && value ? value.replace(/\/+$/, '').replace(/\/v1$/, '') : value;
      }
      return env(key) || options.defaults?.[key] || null;
    }
    function secret(key) {
      if (!secretKeys.includes(key)) return null;
      if (own(settings, key)) return clean(String(settings[key]));
      if (key === secretKeys[0] && own(provider, 'apiKey')) return clean(provider.apiKey);
      const explicit = env(key);
      if (explicit || scoped) return explicit;
      if (key === 'MUSE_DEVICE_TOKEN') {
        const file = env('MUSE_AUTH_PATH') || ctx.host.fs.homeDir + '/.config/muse/auth.json';
        return clean(readJSON(ctx, file)?.providers?.meta?.access_token);
      }
      if (key === 'NOUS_PORTAL_ACCESS_TOKEN') {
        const home = env('HERMES_HOME') || ctx.host.fs.homeDir + '/.hermes';
        for (const file of [home + '/auth.json', home + '/shared/nous_auth.json']) {
          const data = readJSON(ctx, file);
          const state = data?.providers?.nous || data;
          if (!clean(state?.access_token)) continue;
          if (state.expires_at && iso(state.expires_at).getTime() <= Date.parse(ctx.nowIso) + 60000)
            throw failure('authRequired', 'Nous Portal login expired. Run hermes to renew it.');
          return state.access_token.trim();
        }
      }
      return null;
    }
    const cookie = own(provider, 'cookieHeader') ? clean(provider.cookieHeader) : env(definition.id.toUpperCase() + '_COOKIE');
    const rejected = new Set();
    const allowedDomains = definition.cookieDomains || [];
    const domainAllowed = domain => allowedDomains.includes(domain);
    const endpoints = definition.endpoints.map(endpoint => typeof endpoint === 'string'
      ? {url: endpoint, policy: 'https'} : {url: get(endpoint.setting), policy: endpoint.policy});
    const start = Date.now();
    const maxRequests = 40;
    let requests = 0;
    async function request(method, url, requestOptions = {}) {
      if (++requests > maxRequests || Date.now() - start >= 90000)
        throw failure('error', 'Provider request budget exceeded.');
      const validated = ctx.host.http.validateProviderUrl(url, JSON.stringify(endpoints));
      const headers = Object.assign({Accept: 'application/json'}, requestOptions.headers);
      if (definition.auth) {
        const token = secret(definition.auth.secret);
        if (!token) throw failure('authRequired', `Set ${definition.auth.secret} or this provider's apiKey.`);
        const auth = definition.auth;
        const name = auth.type === 'header' ? auth.header : auth.type === 'x-api-key' ? 'x-api-key' : 'Authorization';
        const value = auth.type === 'bearer' ? `Bearer ${token}` : auth.type === 'authorization-scheme' ? `${auth.scheme} ${token}` : token;
        headers[name] = value;
      }
      let bodyText;
      if (requestOptions.form) {
        bodyText = Object.entries(requestOptions.form).map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v)).join('&');
        headers['Content-Type'] = 'application/x-www-form-urlencoded';
      } else if (own(requestOptions, 'body')) {
        bodyText = JSON.stringify(requestOptions.body);
        headers['Content-Type'] = 'application/json';
      }
      const timeout = Math.max(1, Math.min(90, Number(requestOptions.timeoutSeconds) || 15)) * 1000;
      const result = ctx.host.http.request({method, url: validated, headers, bodyText,
        timeoutMs: Math.max(1, Math.min(timeout, 90000 - (Date.now() - start)))});
      return {url: validated, status: result.status, headers: result.headers || {}, bodyText: result.bodyText || ''};
    }
    const cache = new Map();
    const errors = {authenticationExpired: 'authRequired', missingCredential: 'authRequired', permissionDenied: 'credential-denied',
      rateLimited: 'rateLimited', providerUnavailable: 'error', parseFailure: 'error', networkFailure: 'error', apiFailure: 'error'};
    const c = {
      settings: {get, getSecret: secret},
      http: {
        get: (url, opts) => request('GET', url, opts),
        post: (url, opts) => request('POST', url, opts),
        getJSON: async (url, opts) => { const r = await request('GET', url, opts); return {...r, json: JSON.parse(r.bodyText)}; },
        postJSON: async (url, opts) => { const r = await request('POST', url, opts); return {...r, json: JSON.parse(r.bodyText)}; },
      },
      browser: {
        availability: domain => domainAllowed(domain) && cookie && settings.cookies !== 'off' ? 'manual' : 'off',
        rejectCookie: domain => rejected.add(domain),
        cookieHeader: async domain => {
          if (!domainAllowed(domain) || !cookie || rejected.has(domain) || settings.cookies === 'off')
            throw failure('authRequired', 'Import or configure a cookie for ' + domain + '.');
          return cookie;
        },
        sessions: async function* (domain) {
          if (domainAllowed(domain) && cookie && !rejected.has(domain) && settings.cookies !== 'off')
            yield {id: 'configured', header: cookie, source: 'manual', origin: 'https://' + domain};
        },
      },
      date: {
        now: () => iso(ctx.nowIso), iso,
        unixSeconds: value => iso(value * 1000), unixMillis: iso,
        nextDailyReset: (zone, hour) => {
          if (zone !== 'UTC' || !Number.isInteger(hour) || hour < 0 || hour > 23)
            throw failure('error', 'Unsupported reset timezone or hour.');
          const now = iso(ctx.nowIso), next = iso(ctx.nowIso);
          next.setUTCHours(hour, 0, 0, 0);
          if (next <= now) next.setUTCDate(next.getUTCDate() + 1);
          return next;
        },
      },
      format: {
        number, usd: value => '$' + number(value, {minimumFractionDigits: 2, maximumFractionDigits: 2}),
        currency: (value, currency) => currency + ' ' + number(value, {minimumFractionDigits: 2, maximumFractionDigits: 2}),
        monthDay: value => iso(value).toISOString().slice(0, 10),
      },
      fail: Object.fromEntries(Object.entries(errors).map(([name, code]) => [name, message => failure(code, message)])),
      cache: {get: key => cache.get(key), set: (key, value) => { if (cache.size < 64) cache.set(key, value); }},
      env: {timeZone: 'UTC'},
      pct: (used, limit) => finite(used) && finite(limit) && limit > 0 ? Math.min(100, Math.max(0, used / limit * 100)) : 0,
      amountFromPercent: (percent, limit) => percent * limit / 100,
      isDetailLabel: value => typeof value === 'string' && value.trim().length > 0 && value.length <= 256 && !/[\x00-\x1f\x7f]/.test(value),
      log: () => {},
    };
    return c;
  }
  function result(ctx, definition, options, fetched) {
    const usage = fetched?.usage || fetched;
    if (!usage || typeof usage !== 'object') throw failure('error', 'Provider returned no usage.');
    const lines = [];
    function window(label, value) {
      if (!value) return;
      if (finite(value.usedPercent)) {
        lines.push(ctx.line.progress({label, used: Math.max(0, value.usedPercent), limit: 100,
          resetsAt: value.resetsAt ? iso(value.resetsAt).toISOString() : undefined,
          periodDurationMs: finite(value.windowMinutes) && value.windowMinutes > 0 ? value.windowMinutes * 60000 : undefined,
          detail: value.resetDescription || undefined}));
      } else if (value.resetDescription || value.resetsAt) {
        lines.push(ctx.line.text({label, value: value.resetDescription || 'Usage unavailable',
          subtitle: value.resetsAt ? iso(value.resetsAt).toISOString() : undefined}));
      }
    }
    [usage.primary, usage.secondary, usage.tertiary].forEach((value, index) => window(options.labels[index], value));
    for (const extra of usage.extraWindows || []) {
      const value = {...(extra.window || extra)};
      if (extra.usageKnown === false) delete value.usedPercent;
      window(extra.title || extra.id, value);
    }
    if (usage.cost && finite(usage.cost.used)) {
      const cost = usage.cost;
      const label = cost.period || 'Spend';
      // A billing cap is not necessarily an allowance. Only explicit windows become quota meters.
      lines.push(ctx.line.text({label, value: cost.currency + ' ' + number(cost.used, {minimumFractionDigits: 2}),
        subtitle: finite(cost.limit) && cost.limit > 0 ? 'Budget: ' + cost.currency + ' ' + number(cost.limit) : undefined}));
      if (finite(cost.balance)) lines.push(ctx.line.text({label: 'Balance', value: cost.currency + ' ' + number(cost.balance, {minimumFractionDigits: 2})}));
    }
    for (const section of usage.details || []) {
      for (const row of section.rows || []) lines.push(ctx.line.text({label: row.label, value: row.value,
        subtitle: row.secondaryValue || undefined}));
      if (section.chart?.points?.length) lines.push(ctx.line.barChart({label: section.chart.title || section.title || 'Usage', points: section.chart.points}));
    }
    if (usage.identity?.organization) lines.push(ctx.line.text({label: 'Organization', value: usage.identity.organization}));
    if (usage.subscriptionRenewsAt) lines.push(ctx.line.text({label: 'Renews', value: iso(usage.subscriptionRenewsAt).toISOString()}));
    if (usage.subscriptionExpiresAt) lines.push(ctx.line.text({label: 'Expires', value: iso(usage.subscriptionExpiresAt).toISOString()}));
    return {displayName: definition.name, plan: usage.identity?.loginMethod, source: ctx.sourceMode === 'auto' ? options.autoMode || 'api' : ctx.sourceMode,
      lines, ...(usage.empty || !lines.length ? {state: 'no-data'} : {})};
  }
  globalThis.defineProvider = definition => {
    const options = globalThis.__usagestat_bundled_options || {labels: ['Usage', 'Weekly', 'Additional'], defaults: {}};
    globalThis.__usagestat_plugin = {
      id: definition.id,
      probe: async ctx => result(ctx, definition, options, await definition.fetchUsage(context(ctx, definition, options))),
    };
  };
})();
