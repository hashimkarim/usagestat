(function () {
  var USAGE_URL = "https://api.cline.bot/api/v1/users/me/plan/usage-limits";

  function clean(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null;
  }

  function env(ctx, name) {
    try {
      return clean(ctx.host.env.get(name));
    } catch (_) {
      return null;
    }
  }

  function apiKey(ctx) {
    const provider = ctx.provider || {};
    if (Object.prototype.hasOwnProperty.call(provider, "apiKey")) return clean(provider.apiKey);
    const scoped = provider.instanceId && provider.instanceId !== 'clinepass';
    if (scoped) return null;
    const explicit = env(ctx, "CLINEPASS_API_KEY") || env(ctx, "CLINE_API_KEY");
    if (explicit) return explicit;
    const settings = provider.settings || {};
    const dir = env(ctx, 'CLINE_DATA_DIR') || (env(ctx, 'CLINE_DIR') || ctx.host.fs.homeDir + '/.cline') + '/data';
    const file = clean(settings.authPath) || env(ctx, 'CLINE_PROVIDER_SETTINGS_PATH') || dir + '/settings/providers.json';
    let saved;
    try { saved = JSON.parse(ctx.host.fs.readText(file))?.providers?.cline?.settings; } catch (_) { return null; }
    const access = clean(saved?.auth?.accessToken);
    if (access) {
      ctx.clineAuthSource = 'oauth';
      return access.startsWith('workos:') ? access : 'workos:' + access;
    }
    return clean(saved?.apiKey) || clean(saved?.auth?.apiKey);
  }

  function windowFor(ctx, entry) {
    var type = String(entry && entry.type || "");
    var minutes = type === "five_hour" ? 5 * 60 : type === "weekly" ? 7 * 24 * 60 : type === "monthly" ? 30 * 24 * 60 : null;
    if (!minutes) return null;
    var label = type === "five_hour" ? "5-hour" : type === "weekly" ? "Weekly" : "Monthly";
    var raw = entry.percentUsed;
    var percent = Number(raw);
    if ((typeof raw !== "number" && typeof raw !== "string") || String(raw).trim() === "" || !Number.isFinite(percent) || percent < 0) throw "Invalid ClinePass usage percentage.";
    var opts = {
      label: label,
      used: Math.max(0, Math.min(100, percent)),
      limit: 100,
      format: { kind: "percent" },
      periodDurationMs: minutes * 60 * 1000,
    };
    var iso = ctx.util.toIso(entry.resetsAt);
    if (iso) opts.resetsAt = iso;
    return ctx.line.progress(opts);
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "ClinePass credentials missing. Set an API key or run cline auth to sign in.";

    var result = ctx.util.requestJson({
      method: "GET",
      url: USAGE_URL,
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    if (ctx.util.isAuthStatus(result.resp.status)) throw "ClinePass API key invalid or expired.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "ClinePass API error: HTTP " + result.resp.status + ".";
    if (!result.json || !result.json.success || !result.json.data || !Array.isArray(result.json.data.limits)) {
      throw "Failed to parse ClinePass usage.";
    }

    var ordered = ["five_hour", "weekly", "monthly"];
    var metrics = [];
    for (var i = 0; i < ordered.length; i++) {
      for (var j = 0; j < result.json.data.limits.length; j++) {
        if (result.json.data.limits[j] && result.json.data.limits[j].type === ordered[i]) {
          var line = windowFor(ctx, result.json.data.limits[j]);
          if (line) metrics.push(line);
        }
      }
    }
    if (!metrics.length) throw "ClinePass response missing five_hour window.";
    return { displayName: "ClinePass", source: ctx.clineAuthSource || "api", plan: ctx.clineAuthSource ? "Browser" : "API key", lines: metrics };
  }

  globalThis.__openusage_plugin = { id: "clinepass", probe: probe };
})();
