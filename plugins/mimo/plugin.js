(function () {
  var DEFAULT_API_BASE = "https://platform.xiaomimimo.com/api/v1";

  function trim(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null;
  }

  function env(ctx, name) {
    try {
      return trim(ctx.host.env.get(name));
    } catch (_) {
      return null;
    }
  }

  function setting(ctx, names) {
    var settings = ctx.provider && ctx.provider.settings ? ctx.provider.settings : {};
    for (var i = 0; i < names.length; i++) {
      var value = trim(settings[names[i]]);
      if (value) return value;
    }
    return null;
  }

  function apiKey(ctx) {
    return trim(ctx.provider && ctx.provider.apiKey) || env(ctx, "MIMO_API_KEY");
  }

  function tokenPlanRegion(ctx) {
    var region = (trim(ctx.provider && ctx.provider.region) || env(ctx, "MIMO_TOKEN_PLAN_REGION") || "cn").toLowerCase();
    if (region !== "cn" && region !== "sgp") throw "Xiaomi MiMo API region must be cn or sgp.";
    return region;
  }

  function invalidTokenPlan() {
    throw "Xiaomi MiMo returned an unrecognized Token Plan response.";
  }

  function plainObject(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value) ? value : invalidTokenPlan();
  }

  function credits(value) {
    return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : invalidTokenPlan();
  }

  // Xiaomi documents these timezone-less timestamps as UTC on every regional endpoint.
  function tokenPlanDate(value) {
    if (value === undefined || value === null) return null;
    if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(value)) return invalidTokenPlan();
    var iso = value.replace(" ", "T") + "Z";
    var parsed = new Date(iso);
    if (!Number.isFinite(parsed.getTime()) || parsed.toISOString().replace(".000", "") !== iso) return invalidTokenPlan();
    return parsed;
  }

  function probeTokenPlan(ctx, key) {
    var region = tokenPlanRegion(ctx);
    var resp = ctx.util.request({
      method: "GET",
      url: "https://token-plan-" + region + ".xiaomimimo.com/v1/platform/token_plan",
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    if (resp.status === 401 || resp.status === 403) {
      throw { code: "credential-denied", message: "Xiaomi MiMo rejected the API key (HTTP " + resp.status + "). Check your Token Plan API key." };
    }
    if (resp.status === 429) throw "Xiaomi MiMo is rate limiting requests. Try again later.";
    if (resp.status !== 200) throw "Xiaomi MiMo request failed: HTTP " + resp.status + ". Try refreshing later.";

    var root = plainObject(ctx.util.tryParseJson(resp.bodyText));
    if (root.object !== "token_plan") invalidTokenPlan();
    var quota = plainObject(root.quota);
    var total = credits(quota.total_credits);
    var used = credits(quota.used_credits);
    var remaining = credits(quota.remaining_credits);
    if (remaining !== Math.max(0, total - used)) invalidTokenPlan();
    var period = plainObject(root.period);
    var start = tokenPlanDate(period.start_at);
    var end = tokenPlanDate(period.end_at);
    if (start && end && end <= start) invalidTokenPlan();

    var lines = [];
    if (total > 0) {
      var progress = {
        label: "Credits",
        used: used,
        limit: total,
        format: { kind: "count", suffix: "credits" },
        detail: used + "/" + total + " credits",
      };
      if (end) progress.resetsAt = end.toISOString();
      // A partial-minute period is not rounded into a window length.
      if (start && end && (end.getTime() - start.getTime()) % 60000 === 0) {
        progress.periodDurationMs = end.getTime() - start.getTime();
      }
      lines.push(ctx.line.progress(progress));
    } else {
      lines.push(ctx.line.badge({ label: "Token plan", text: "No quota", color: "#a3a3a3" }));
    }
    var plan = typeof root.plan_code === "string" && root.plan_code.trim() ? root.plan_code.trim() : null;
    return { displayName: "Xiaomi MiMo", source: "api", plan: plan, lines: lines };
  }

  function cookieHeader(ctx) {
    var raw = trim(ctx.provider && ctx.provider.cookieHeader) ||
      setting(ctx, ["cookieHeader", "cookie"]) ||
      env(ctx, "MIMO_COOKIE") ||
      env(ctx, "XIAOMI_MIMO_COOKIE");
    var normalized = normalizeCookie(raw);
    if (!normalized) {
      throw "Xiaomi MiMo requires api-platform_serviceToken and userId cookies.";
    }
    return normalized;
  }

  function normalizeCookie(raw) {
    var known = {
      "api-platform_serviceToken": true,
      userId: true,
      "api-platform_ph": true,
      "api-platform_slh": true,
    };
    var pairs = [];
    var foundToken = false;
    var foundUser = false;
    String(raw || "").split(";").forEach(function (chunk) {
      var idx = chunk.indexOf("=");
      if (idx < 0) return;
      var name = chunk.slice(0, idx).trim();
      var value = chunk.slice(idx + 1).trim();
      if (!known[name] || !value) return;
      if (name === "api-platform_serviceToken") foundToken = true;
      if (name === "userId") foundUser = true;
      pairs.push({ name: name, value: value });
    });
    if (!foundToken || !foundUser) return null;
    pairs.sort(function (a, b) { return a.name < b.name ? -1 : a.name > b.name ? 1 : 0; });
    return pairs.map(function (pair) { return pair.name + "=" + pair.value; }).join("; ");
  }

  function apiBase(ctx) {
    return (setting(ctx, ["apiUrl", "apiBase", "baseUrl"]) || env(ctx, "MIMO_API_URL") || DEFAULT_API_BASE).replace(/\/+$/, "");
  }

  function requestJson(ctx, base, path, cookie) {
    var resp = ctx.util.request({
      method: "GET",
      url: base + "/" + path,
      headers: {
        Cookie: cookie,
        Accept: "application/json, text/plain, */*",
        "Accept-Language": "en-US,en;q=0.9",
        "x-timeZone": "UTC+01:00",
        Origin: "https://platform.xiaomimimo.com",
        Referer: "https://platform.xiaomimimo.com/#/console/balance",
        "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
      },
      timeoutMs: 15000,
    });
    if (ctx.util.isAuthStatus(resp.status)) throw "Xiaomi MiMo browser session expired. Log in again.";
    if (resp.status < 200 || resp.status >= 300) throw "Xiaomi MiMo request failed: HTTP " + resp.status + ".";
    var json = ctx.util.tryParseJson(resp.bodyText);
    if (!json || typeof json !== "object") throw "Could not parse Xiaomi MiMo response.";
    if (json.code === 401) throw "Xiaomi MiMo login required.";
    if (json.code === 403) throw "Xiaomi MiMo browser session expired. Log in again.";
    if (json.code !== 0) {
      var message = typeof json.message === "string" && json.message.trim() ? json.message.trim() : "code " + json.code;
      throw "Could not parse Xiaomi MiMo balance: " + message;
    }
    return json.data || null;
  }

  function optionalJson(ctx, base, path, cookie) {
    try {
      return requestJson(ctx, base, path, cookie);
    } catch (e) {
      ctx.host.log.warn("MiMo optional endpoint failed: " + path + ": " + String(e));
      return null;
    }
  }

  function numberValue(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value);
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function parseMimoDate(ctx, value) {
    var text = trim(value);
    if (!text) return null;
    return ctx.util.toIso(text.indexOf("T") >= 0 ? text : text.replace(" ", "T") + "Z");
  }

  function balanceText(balance, currency, cashBalance, giftBalance) {
    var total = balance.toFixed(2) + " " + currency;
    var cash = numberValue(cashBalance);
    var gift = numberValue(giftBalance);
    if (cash === null || gift === null) return total;
    return total + " (Paid " + cash.toFixed(2) + " / Granted " + gift.toFixed(2) + ")";
  }

  function probe(ctx) {
    // API failures stay visible; they never switch to a browser account.
    var key = ctx.sourceMode === "web" ? null : apiKey(ctx);
    if (key) return probeTokenPlan(ctx, key);
    if (ctx.sourceMode === "api") {
      throw { code: "missing-auth", message: "Set a Xiaomi MiMo Token Plan API key (MIMO_API_KEY)." };
    }
    var cookie = cookieHeader(ctx);
    var base = apiBase(ctx);
    var balanceData = requestJson(ctx, base, "balance", cookie);
    if (!balanceData || typeof balanceData !== "object") throw "Could not parse Xiaomi MiMo balance: missing payload.";
    var balance = numberValue(balanceData.balance);
    var currency = trim(balanceData.currency);
    if (balance === null || !currency) throw "Could not parse Xiaomi MiMo balance: invalid balance payload.";

    var detail = optionalJson(ctx, base, "tokenPlan/detail", cookie);
    var usage = optionalJson(ctx, base, "tokenPlan/usage", cookie);
    var lines = [];
    var item = usage &&
      usage.monthUsage &&
      Array.isArray(usage.monthUsage.items) &&
      usage.monthUsage.items.length > 0
      ? usage.monthUsage.items[0]
      : null;
    var periodEnd = detail && parseMimoDate(ctx, detail.currentPeriodEnd);
    if (item) {
      var used = numberValue(item.used) || 0;
      var limit = numberValue(item.limit) || 0;
      var progress = {
        label: "Tokens",
        used: used,
        limit: limit > 0 ? limit : 1,
        format: { kind: "count", suffix: "tokens" },
        detail: Math.round(used) + "/" + Math.round(limit) + " tokens",
      };
      if (periodEnd) progress.resetsAt = periodEnd;
      lines.push(ctx.line.progress(progress));
    } else {
      lines.push(ctx.line.badge({ label: "Token plan", text: "No usage", color: "#a3a3a3" }));
    }
    lines.push(ctx.line.text({
      label: "Balance",
      value: balanceText(balance, currency, balanceData.cashBalance, balanceData.giftBalance),
    }));

    var plan = detail && typeof detail.planCode === "string" && detail.planCode.trim()
      ? detail.planCode.trim()
      : balance.toFixed(2) + " " + currency;
    return {
      displayName: "Xiaomi MiMo",
      source: "web",
      plan: plan,
      lines: lines,
    };
  }

  globalThis.__openusage_plugin = { id: "mimo", probe: probe };
})();
