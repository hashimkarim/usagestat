(function () {
  var BASE = "https://zenmux.ai/api/v1/management";

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
    return clean(ctx.provider && ctx.provider.apiKey) || env(ctx, "ZENMUX_MANAGEMENT_API_KEY") || env(ctx, "ZENMUX_API_KEY");
  }

  function requestJson(ctx, path, key, timeoutMs) {
    var result = ctx.util.requestJson({
      method: "GET",
      url: BASE + "/" + path,
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: timeoutMs || 15000,
    });
    if (ctx.util.isAuthStatus(result.resp.status)) throw "ZenMux Management API key invalid or expired.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "ZenMux Management API returned HTTP " + result.resp.status + ".";
    if (!result.json || typeof result.json !== "object") throw "Failed to parse ZenMux response.";
    return result.json;
  }

  function iso(ctx, value) {
    return value ? ctx.util.toIso(value) : null;
  }

  function title(value) {
    var v = String(value || "").trim();
    return v ? v.charAt(0).toUpperCase() + v.slice(1) : "";
  }

  function amount(value) {
    var n = number(value);
    if (n === null) return null;
    return Math.abs(n - Math.round(n)) < 0.001 ? String(Math.round(n)) : n.toFixed(2);
  }

  function number(raw) {
    if ((typeof raw !== "number" && typeof raw !== "string") || String(raw).trim() === "") return null;
    var n = Number(raw);
    return Number.isFinite(n) ? n : null;
  }

  function quotaLine(ctx, label, quota, minutes) {
    if (!quota) return null;
    var percent = number(quota.usage_percentage);
    if (percent === null || percent < 0) throw "Invalid ZenMux usage percentage.";
    var opts = {
      label: label,
      used: Math.max(0, Math.min(100, percent * 100)),
      limit: 100,
      format: { kind: "percent" },
      periodDurationMs: minutes * 60 * 1000,
    };
    if (amount(quota.used_flows) !== null && amount(quota.max_flows) !== null) opts.detail = amount(quota.used_flows) + " / " + amount(quota.max_flows) + " flows";
    var reset = iso(ctx, quota.resets_at);
    if (reset) opts.resetsAt = reset;
    return ctx.line.progress(opts);
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "Missing ZenMux Management API key. Set ZENMUX_MANAGEMENT_API_KEY, ZENMUX_API_KEY, or provider apiKey.";
    var sub = requestJson(ctx, "subscription/detail", key);
    if (!sub.success || !sub.data) throw "ZenMux subscription response reported failure.";
    var data = sub.data;
    var plan = data.plan && title(data.plan.tier);
    var status = title(data.account_status);
    var planLabel = plan ? plan + " plan" : null;
    if (status && status.toLowerCase() !== "healthy") planLabel = planLabel ? planLabel + " · " + status : status;
    var metrics = [
      quotaLine(ctx, "5-hour quota", data.quota_5_hour, 5 * 60),
      quotaLine(ctx, "Weekly quota", data.quota_7_day, 7 * 24 * 60),
    ].filter(function (line) { return line !== null; });
    var expiresAt = iso(ctx, data.plan && data.plan.expires_at);
    if (expiresAt) metrics.push(ctx.line.text({ label: "Subscription expires", value: expiresAt }));

    try {
      var bal = requestJson(ctx, "payg/balance", key, 3000);
      if (bal.success && bal.data && number(bal.data.total_credits) !== null && String(bal.data.currency || "").toUpperCase() === "USD") {
        metrics.push(ctx.line.text({ label: "PAYG Balance", value: "$" + number(bal.data.total_credits).toFixed(2) }));
      }
    } catch (error) {
      ctx.host.log.warn("ZenMux PAYG balance request failed: " + String(error));
    }

    if (!metrics.length) throw "ZenMux returned no measurable usage or balance.";
    return { displayName: "ZenMux", source: "api", plan: planLabel, lines: metrics };
  }

  globalThis.__openusage_plugin = { id: "zenmux", probe: probe };
})();
