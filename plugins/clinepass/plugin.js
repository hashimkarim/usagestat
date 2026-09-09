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
    return clean(ctx.provider && ctx.provider.apiKey) || env(ctx, "CLINEPASS_API_KEY") || env(ctx, "CLINE_API_KEY");
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
    if (!key) throw "ClinePass API key not found. Set CLINEPASS_API_KEY, CLINE_API_KEY, or provider apiKey.";

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
    return { displayName: "ClinePass", source: "api", plan: "API key", lines: metrics };
  }

  globalThis.__openusage_plugin = { id: "clinepass", probe: probe };
})();
