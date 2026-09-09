(function () {
  function clean(value) {
    if (typeof value !== "string") return null;
    var v = value.trim();
    if (!v) return null;
    if ((v[0] === "\"" && v[v.length - 1] === "\"") || (v[0] === "'" && v[v.length - 1] === "'")) v = v.slice(1, -1).trim();
    return v || null;
  }

  function env(ctx, name) {
    try {
      return clean(ctx.host.env.get(name));
    } catch (_) {
      return null;
    }
  }

  function setting(ctx, names) {
    var settings = ctx.provider && ctx.provider.settings ? ctx.provider.settings : {};
    for (var i = 0; i < names.length; i++) {
      var v = clean(settings[names[i]]);
      if (v) return v;
    }
    return null;
  }

  function apiKey(ctx) {
    return clean(ctx.provider && ctx.provider.apiKey) || env(ctx, "SUB2API_API_KEY");
  }

  function baseUrl(ctx) {
    var raw = setting(ctx, ["baseUrl", "apiUrl", "apiBase"]) || clean(ctx.provider && ctx.provider.workspaceId) || env(ctx, "SUB2API_BASE_URL");
    var base = raw ? ctx.host.http.validateBaseUrl(raw, true) : null;
    if (!base) throw "Missing or invalid sub2api base URL. Set SUB2API_BASE_URL or provider workspaceId.";
    return base;
  }

  function usageUrl(base) {
    var path = base.replace(/^https?:\/\/[^/]+/, "");
    if (/\/v1\/usage$/i.test(path)) return base;
    if (/\/v1$/i.test(path)) return base + "/usage";
    return base + "/v1/usage";
  }

  function number(value, fallback) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return fallback === undefined ? null : fallback;
  }

  function percent(used, limit) {
    return limit > 0 ? Math.max(0, Math.min(100, used / limit * 100)) : 0;
  }

  function money(value, unit) {
    return String(unit || "USD").toUpperCase() === "USD" ? "$" + value.toFixed(2) : value.toFixed(2) + " " + unit;
  }

  function dateIso(ctx, value) {
    return value ? ctx.util.toIso(value) : null;
  }

  function progress(ctx, label, used, limit, unit, minutes, reset) {
    if (used === null || used < 0 || limit === null || limit < 0) throw "sub2api returned invalid " + label + " quota values.";
    if (limit === 0) return ctx.line.text({ label: label, value: money(used, unit) + " used; no cap reported" });
    var opts = {
      label: label,
      used: percent(used, limit),
      limit: 100,
      format: { kind: "percent" },
      detail: money(used, unit) + " / " + money(limit, unit),
    };
    if (minutes) opts.periodDurationMs = minutes * 60 * 1000;
    var iso = dateIso(ctx, reset);
    if (iso) opts.resetsAt = iso;
    return ctx.line.progress(opts);
  }

  function windowMinutes(window) {
    switch (String(window || "").toLowerCase()) {
      case "5h": return 5 * 60;
      case "1d": return 24 * 60;
      case "7d": return 7 * 24 * 60;
      default: return null;
    }
  }

  function windowTitle(window) {
    switch (String(window || "").toLowerCase()) {
      case "5h": return "5 hour limit";
      case "1d": return "Daily limit";
      case "7d": return "7 day limit";
      default: return String(window || "Rate") + " limit";
    }
  }

  function totalsLine(ctx, label, totals, unit) {
    var requests = number(totals && totals.requests);
    var tokens = number(totals && totals.total_tokens);
    var cost = number(totals && totals.actual_cost);
    if (requests === null || tokens === null || cost === null || requests < 0 || tokens < 0 || cost < 0) return null;
    return ctx.line.text({
      label: label,
      value: requests + " requests, " + tokens + " tokens, " + money(cost, unit),
    });
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "Missing sub2api API key. Set SUB2API_API_KEY or provider apiKey.";
    var url = usageUrl(baseUrl(ctx));
    var sep = url.indexOf("?") >= 0 ? "&" : "?";
    url += sep + "days=30&timezone=UTC";
    var result = ctx.util.requestJson({
      method: "GET",
      url: url,
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    if (ctx.util.isAuthStatus(result.resp.status)) throw "sub2api API key invalid or expired.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "sub2api usage returned HTTP " + result.resp.status + ".";
    var data = result.json;
    if (!data || typeof data !== "object") throw "Could not parse sub2api usage.";
    if (data.isValid === false) throw "sub2api API key invalid or expired.";

    var unit = data.unit || (data.quota && data.quota.unit) || "USD";
    var metrics = [];
    if (data.subscription) {
      var sub = data.subscription;
      ["daily", "weekly", "monthly"].forEach(function (window, index) {
        var used = number(sub[window + "_usage_usd"]);
        var limit = number(sub[window + "_limit_usd"]);
        var label = ["Daily", "Weekly", "Monthly"][index];
        if (used === null) return;
        if (used < 0) throw "sub2api returned negative usage.";
        metrics.push(limit !== null ? progress(ctx, label, used, limit, unit, [1440, 10080, 43200][index], null)
          : ctx.line.text({ label: label, value: money(used, unit) + " used" }));
      });
      if (number(data.balance) !== null) metrics.push(ctx.line.text({ label: "Balance", value: money(number(data.balance), unit) }));
    } else if (data.quota) {
      metrics.push(progress(ctx, "Quota", number(data.quota.used), number(data.quota.limit), data.quota.unit || unit, null, null));
    } else if (Array.isArray(data.rate_limits) && data.rate_limits.length) {
      var first = data.rate_limits[0];
      metrics.push(progress(ctx, windowTitle(first.window), number(first.used), number(first.limit), unit, windowMinutes(first.window), first.reset_at));
    } else if (data.balance !== undefined || data.remaining !== undefined) {
      var balance = number(data.balance !== undefined ? data.balance : data.remaining);
      if (balance === null) throw "sub2api returned an invalid balance.";
      metrics.push(ctx.line.text({ label: "Balance", value: money(balance, unit) }));
    } else {
      metrics.push(ctx.line.badge({ label: "Status", text: "No quota data", color: "#a3a3a3" }));
    }

    if (Array.isArray(data.rate_limits)) {
      var skipFirst = !data.quota && !data.subscription && data.rate_limits.length > 0;
      for (var i = 0; i < data.rate_limits.length; i++) {
        if (skipFirst && i === 0) continue;
        var rl = data.rate_limits[i];
        metrics.push(progress(ctx, windowTitle(rl.window), number(rl.used), number(rl.limit), unit, windowMinutes(rl.window), rl.reset_at));
      }
    }
    if (data.usage && data.usage.today) {
      var today = totalsLine(ctx, "Today", data.usage.today, unit);
      if (today) {
        metrics.push(today);
        if (String(unit).toUpperCase() === "USD") {
          try {
            ctx.host.usageDaily.ingest({ source: "sub2api_api", daily: [{ date: ctx.nowIso.slice(0, 10),
              costUsd: number(data.usage.today.actual_cost), totalTokens: number(data.usage.today.total_tokens) }] });
          } catch (_) { ctx.host.log.warn("Could not persist sub2api daily usage"); }
        }
      }
    }
    if (data.usage && data.usage.total) {
      var total = totalsLine(ctx, "Total", data.usage.total, unit);
      if (total) metrics.push(total);
    }
    var expires = ctx.util.toIso(data.subscription && data.subscription.expires_at || data.expires_at);
    if (expires) metrics.push(ctx.line.text({ label: "Expires", value: expires }));
    if (!metrics.length) throw "sub2api returned no measurable quota or usage.";

    var plan = clean(data.planName);
    if (plan && data.mode && data.mode !== "unknown" && String(data.mode).toLowerCase() !== plan.toLowerCase()) plan += " (" + data.mode + ")";
    else if (!plan && data.mode && data.mode !== "unknown") plan = data.mode;
    return { displayName: "sub2api", source: "api", plan: plan, lines: metrics };
  }

  globalThis.__openusage_plugin = { id: "sub2api", probe: probe };
})();
