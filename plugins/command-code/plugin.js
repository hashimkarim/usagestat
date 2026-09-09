(function () {
  var AUTH_FILE = "~/.commandcode/auth.json";
  var API_BASE = "https://api.commandcode.ai";
  var ERR_NOT_LOGGED_IN = "Not logged in. Run `cmd login` to authenticate.";

  var PLAN_LABELS = {
    "individual-go": "Individual Go",
    "individual-goat": "GOAT",
    "individual-pro": "Pro",
    "individual-pro-v1": "Pro",
    "individual-max": "Max",
    "individual-ultra": "Ultra",
    "individual": "Individual",
    "pro": "Pro",
    "team": "Team",
    "enterprise": "Enterprise",
  };

  function readNumber(value) {
    if (typeof value !== "number" && typeof value !== "string") return null;
    if (typeof value === "string" && !value.trim()) return null;
    var n = Number(value);
    return Number.isFinite(n) ? n : null;
  }

  function trimString(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null;
  }

  function loadApiKeyFromEnv(ctx) {
    try {
      return trimString(ctx.host.env.get("COMMAND_CODE_API_KEY"));
    } catch (e) {
      ctx.host.log.warn("COMMAND_CODE_API_KEY read failed: " + String(e));
      return null;
    }
  }

  function loadApiKeyFromSettings(ctx) {
    var settings = ctx.provider && ctx.provider.settings ? ctx.provider.settings : {};
    return trimString(settings.apiKey) ||
      trimString(settings.token) ||
      (ctx.provider && trimString(ctx.provider.apiKey));
  }

  function loadApiKeyFromFile(ctx) {
    if (!ctx.host.fs.exists(AUTH_FILE)) return null;
    try {
      var text = ctx.host.fs.readText(AUTH_FILE);
      var parsed = ctx.util.tryParseJson(text);
      if (!parsed || typeof parsed !== "object") return null;
      return trimString(parsed.apiKey);
    } catch (e) {
      ctx.host.log.warn("Command Code auth file read failed: " + String(e));
      return null;
    }
  }

  function loadApiKey(ctx) {
    return loadApiKeyFromSettings(ctx) || loadApiKeyFromEnv(ctx) || loadApiKeyFromFile(ctx);
  }

  function apiCall(ctx, url, apiKey) {
    try {
      var resp = ctx.host.http.request({
        method: "GET",
        url: url,
        headers: {
          Authorization: "Bearer " + apiKey,
          "Content-Type": "application/json",
          Accept: "application/json",
          "User-Agent": "OpenUsage",
        },
        timeoutMs: 10000,
      });
      if (resp.status < 200 || resp.status >= 300) {
        ctx.host.log.warn("Command Code API call failed: " + url + " status=" + resp.status);
        return null;
      }
      var parsed = ctx.util.tryParseJson(resp.bodyText);
      if (!parsed) {
        ctx.host.log.warn("Command Code API response was not valid JSON: " + url);
        return null;
      }
      return parsed;
    } catch (e) {
      ctx.host.log.error("Command Code API call exception: " + url + " " + String(e));
      return null;
    }
  }

  function fmtTokens(n) {
    var abs = Math.abs(n);
    var sign = n < 0 ? "-" : "";
    var units = [
      { threshold: 1e9, divisor: 1e9, suffix: "B" },
      { threshold: 1e6, divisor: 1e6, suffix: "M" },
      { threshold: 1e3, divisor: 1e3, suffix: "K" },
    ];
    for (var i = 0; i < units.length; i++) {
      var unit = units[i];
      if (abs >= unit.threshold) {
        var scaled = abs / unit.divisor;
        var formatted = scaled >= 10 ? Math.round(scaled).toString() : scaled.toFixed(1).replace(/\.0$/, "");
        return sign + formatted + unit.suffix;
      }
    }
    return sign + Math.round(abs).toString();
  }

  function probe(ctx) {
    var apiKey = loadApiKey(ctx);
    if (!apiKey) {
      throw ERR_NOT_LOGGED_IN;
    }

    var whoami = apiCall(ctx, API_BASE + "/alpha/whoami", apiKey);
    if (!whoami) {
      throw "Command Code API unreachable. Check your connection.";
    }

    var creditsResp = apiCall(ctx, API_BASE + "/alpha/billing/credits", apiKey);
    var usageResp = apiCall(ctx, API_BASE + "/alpha/usage/summary", apiKey);
    var subResp = apiCall(ctx, API_BASE + "/alpha/billing/subscriptions", apiKey);

    var creditsRemaining = null;
    if (creditsResp && creditsResp.credits && typeof creditsResp.credits === "object") {
      creditsRemaining = readNumber(creditsResp.credits.monthlyCredits);
    }

    var totalCost = null;
    var monthlyUsed = null;
    var totalTokens = null;
    var models = [];
    if (usageResp && typeof usageResp === "object") {
      totalCost = readNumber(usageResp.totalCost);
      monthlyUsed = readNumber(usageResp.totalMonthlyCredits);
      totalTokens = readNumber(usageResp.totalTokens);
      if (Array.isArray(usageResp.models)) models = usageResp.models;
    }

    var planLabel = null;
    if (subResp && subResp.success && subResp.data && typeof subResp.data.planId === "string") {
      var planId = subResp.data.planId;
      planLabel = PLAN_LABELS[planId] || ctx.fmt.planLabel(planId);
    }

    var lines = [];
    if (creditsRemaining !== null && monthlyUsed !== null) {
      var totalPlan = monthlyUsed + Math.max(0, creditsRemaining);
      var usedPercent = totalPlan > 0 ? (monthlyUsed / totalPlan) * 100 : 100;
      lines.push(ctx.line.progress({
        label: "Monthly credits",
        used: Math.max(0, Math.min(100, Math.round(usedPercent * 10) / 10)),
        limit: 100,
        format: { kind: "percent" },
        periodDurationMs: 30 * 24 * 60 * 60 * 1000,
      }));
    }

    if (totalCost !== null && totalCost > 0) {
      lines.push(ctx.line.text({ label: "Total spent", value: "$" + totalCost.toFixed(2) }));
    }

    if (totalTokens !== null && totalTokens > 0) {
      lines.push(ctx.line.text({ label: "Tokens used", value: fmtTokens(Math.round(totalTokens)) }));
    }

    var modelParts = [];
    for (var i = 0; i < models.length; i++) {
      var m = models[i];
      if (!m || typeof m !== "object") continue;
      var name = m.model || m.name;
      if (typeof name !== "string" || !name.trim()) continue;
      var cost = readNumber(m.totalCost) || 0;
      var count = readNumber(m.count) || 0;
      var shortName = name.indexOf("/") >= 0 ? name.slice(name.indexOf("/") + 1) : name;
      var detail = shortName;
      if (cost > 0) detail += " $" + cost.toFixed(2);
      if (count > 0) detail += " (" + count + " calls)";
      modelParts.push(detail);
    }
    if (modelParts.length > 0) {
      lines.push(ctx.line.text({ label: "Models", value: modelParts.join("  ") }));
    }

    return { plan: planLabel, lines: lines };
  }

  globalThis.__openusage_plugin = { id: "command-code", probe: probe };
})();
