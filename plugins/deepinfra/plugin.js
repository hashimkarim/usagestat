(function () {
  var CHECKLIST_URL = "https://api.deepinfra.com/payment/checklist?compute_owed=true";
  var USAGE_URL = "https://api.deepinfra.com/payment/usage?from=current";

  function clean(value) {
    if (typeof value !== "string") return null;
    var v = value.trim();
    if (!v) return null;
    if ((v[0] === "\"" && v[v.length - 1] === "\"") || (v[0] === "'" && v[v.length - 1] === "'")) v = v.slice(1, -1).trim();
    if (v.slice(0, 7).toLowerCase() === "bearer ") v = v.slice(7).trim();
    return v || null;
  }

  function env(ctx, name) {
    try {
      return clean(ctx.host.env.get(name));
    } catch (_) {
      return null;
    }
  }

  function apiKey(ctx) {
    return clean(ctx.provider && ctx.provider.apiKey) || env(ctx, "DEEPINFRA_API_KEY") || env(ctx, "DEEPINFRA_TOKEN");
  }

  function requestJson(ctx, url, key, label, timeoutMs) {
    var result = ctx.util.requestJson({
      method: "GET",
      url: url,
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: timeoutMs || 15000,
    });
    if (result.resp.status === 401) throw "DeepInfra API key rejected.";
    if (result.resp.status === 403) throw "DeepInfra API key cannot access billing data.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "DeepInfra " + label + " API error: HTTP " + result.resp.status + ".";
    if (!result.json || typeof result.json !== "object") throw "Failed to parse DeepInfra " + label + " response.";
    return result.json;
  }

  function number(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "DeepInfra API key not found. Set DEEPINFRA_API_KEY, DEEPINFRA_TOKEN, or provider apiKey.";

    var checklist = requestJson(ctx, CHECKLIST_URL, key, "billing");
    var recent = number(checklist.recent);
    var stripeBalance = number(checklist.stripe_balance);
    if (recent === null || recent < 0 || stripeBalance === null) throw "DeepInfra returned invalid billing amounts.";
    var currentMonth = null;
    try {
      var usage = requestJson(ctx, USAGE_URL, key, "usage", 3000);
      if (Array.isArray(usage.months) && usage.months.length) {
        var cents = number(usage.months[usage.months.length - 1].total_cost);
        if (cents !== null && cents >= 0) currentMonth = cents / 100;
      }
    } catch (error) {
      ctx.host.log.warn("DeepInfra monthly usage unavailable: " + String(error));
    }
    var netBalance = stripeBalance + recent;
    var available = Math.max(0, -netBalance);
    var owed = Math.max(0, netBalance);
    var limit = number(checklist.limit);
    var suspended = checklist.suspended === true;
    var balanceText = owed > 0 ? "$" + owed.toFixed(2) + " owed" : "$" + available.toFixed(2) + " available";
    var detail = balanceText + (currentMonth === null ? "" : " · $" + currentMonth.toFixed(2) + " spent this month");
    if (suspended) detail = "Suspended" + (checklist.suspend_reason ? ": " + checklist.suspend_reason : "") + " · " + detail;

    var metrics = [];
    if (limit > 0 && Number.isFinite(limit)) {
      metrics.push(ctx.line.progress({
        label: "Billing Cycle",
        used: Math.max(0, recent),
        limit: limit,
        format: { kind: "dollars" },
        detail: detail,
      }));
    } else {
      metrics.push(ctx.line.text({ label: "Balance", value: balanceText }));
    }
    if (currentMonth !== null) metrics.push(ctx.line.text({ label: "Month Cost", value: "$" + currentMonth.toFixed(2) }));
    if (suspended || owed > 0 || available <= 0) {
      metrics.push(ctx.line.badge({ label: "Status", text: suspended ? "Suspended" : "No prepaid balance", color: "#ef4444" }));
    }

    return { displayName: "DeepInfra", source: "api", lines: metrics };
  }

  globalThis.__openusage_plugin = { id: "deepinfra", probe: probe };
})();
