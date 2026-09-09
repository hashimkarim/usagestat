(function () {
  var GLOBAL_API = "https://qoder.com/api/v2/me/usages/big_model_credits";
  var CHINA_API = "https://qoder.com.cn/api/v2/me/usages/big_model_credits";
  var USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

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

  function normalizeCookie(raw) {
    var header = trim(raw);
    if (!header) return null;
    if (header.slice(0, 7).toLowerCase() === "cookie:") header = header.slice(7).trim();
    var pairs = [];
    var chunks = header.split(";");
    for (var i = 0; i < chunks.length; i++) {
      var idx = chunks[i].indexOf("=");
      if (idx < 0) continue;
      var name = chunks[i].slice(0, idx).trim();
      var value = chunks[i].slice(idx + 1).trim();
      if (name && value) pairs.push(name + "=" + value);
    }
    return pairs.length ? pairs.join("; ") : null;
  }

  function cookieHeader(ctx) {
    return normalizeCookie(ctx.provider && ctx.provider.cookieHeader) ||
      normalizeCookie(setting(ctx, ["cookieHeader", "cookie"])) ||
      normalizeCookie(env(ctx, "QODER_COOKIE"));
  }

  function requestRegion(ctx, url, referer, cookie) {
    var result = ctx.util.requestJson({
      method: "GET",
      url: url,
      headers: {
        Cookie: cookie,
        Accept: "application/json, text/plain, */*",
        Referer: referer,
        "User-Agent": USER_AGENT,
      },
      timeoutMs: 15000,
    });
    if (ctx.util.isAuthStatus(result.resp.status)) throw { auth: true, message: "Qoder session expired." };
    if (result.resp.status < 200 || result.resp.status >= 300) throw "Qoder usage returned HTTP " + result.resp.status + ".";
    if (!result.json || typeof result.json !== "object") throw "Failed to parse Qoder usage.";
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

  function first(obj, keys) {
    if (!obj || typeof obj !== "object" || Array.isArray(obj)) return undefined;
    for (var i = 0; i < keys.length; i++) {
      if (obj[keys[i]] !== undefined && obj[keys[i]] !== null) return obj[keys[i]];
    }
    return undefined;
  }

  function stringFromKeys(obj, keys) {
    var v = first(obj, keys);
    return typeof v === "string" && v.trim() ? v.trim() : null;
  }

  function numberFromKeys(obj, keys) {
    return number(first(obj, keys));
  }

  function parseDate(ctx, value) {
    if (value === null || value === undefined || value === "") return null;
    return ctx.util.toIso(value);
  }

  function normalizedPercent(value) {
    var n = number(value);
    if (n === null) return null;
    if (n <= 1 && n >= 0) n *= 100;
    return Math.max(0, Math.min(100, n));
  }

  function usagePercentage(used, total, remaining, provided) {
    if (used < 0 || total < 0 || remaining < 0) throw "Qoder quota values must be nonnegative.";
    if (total === 0) {
      if (used !== 0 || remaining !== 0) throw "Qoder zero total quota has nonzero usage.";
      return provided === null ? 100 : normalizedPercent(provided);
    }
    return provided === null ? used / total * 100 : normalizedPercent(provided);
  }

  function quotaSummary(root, containerKeys) {
    var container = first(root, containerKeys);
    if (!container || typeof container !== "object" || Array.isArray(container)) return null;
    var summary = container.quotaSummary || container.quota_summary;
    if (!summary || typeof summary !== "object" || Array.isArray(summary)) return null;
    var used = numberFromKeys(summary, ["usedValue", "used_value"]);
    var total = numberFromKeys(summary, ["limitValue", "limit_value"]);
    if (used === null || total === null) throw "Missing Qoder quota summary values.";
    var remaining = numberFromKeys(summary, ["remainingValue", "remaining_value"]);
    if (remaining === null) remaining = Math.max(0, total - used);
    var provided = numberFromKeys(summary, ["usagePercentage", "usage_percentage"]);
    var unitRaw = stringFromKeys(summary, ["unit"]);
    return {
      used: used,
      total: total,
      remaining: remaining,
      percentage: usagePercentage(used, total, remaining, provided),
      unit: unitRaw && /^credits?$/i.test(unitRaw) ? "credits" : "units",
    };
  }

  function mergeSummaries(a, b) {
    var used = a.used + b.used;
    var total = a.total + b.total;
    var remaining = a.remaining + b.remaining;
    return {
      used: used,
      total: total,
      remaining: remaining,
      percentage: usagePercentage(used, total, remaining, null),
      unit: a.unit || b.unit || "credits",
    };
  }

  function quotaSummarySnapshot(ctx, root, loginMethod) {
    var base = quotaSummary(root, ["totalQuota", "total_quota"]);
    if (!base) return null;
    var shared = quotaSummary(root, ["sharedQuota", "shared_quota"]);
    var merged = shared ? mergeSummaries(base, shared) : base;
    var reset = parseDate(ctx, first(root, ["nextResetAt", "next_reset_at"]));
    var detail = Math.round(merged.used) + "/" + Math.round(merged.total) + " " + (merged.unit || "credits") +
      " used, " + Math.round(merged.remaining) + " remaining";
    var opts = {
      label: "Credits",
      used: Math.max(0, Math.min(100, merged.percentage)),
      limit: 100,
      format: { kind: "percent" },
      detail: detail,
    };
    if (reset) opts.resetsAt = reset;
    return { displayName: "Qoder", source: "web", plan: loginMethod, lines: [ctx.line.progress(opts)] };
  }

  function collectCreditWindows(ctx, value, out) {
    if (!value || typeof value !== "object") return;
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) collectCreditWindows(ctx, value[i], out);
      return;
    }
    var used = numberFromKeys(value, ["used", "usedCredits", "used_credits", "usage", "usedQuota", "used_quota"]);
    var total = numberFromKeys(value, ["total", "totalCredits", "total_credits", "limit", "quota", "quotaLimit", "quota_limit"]);
    var percent = normalizedPercent(first(value, ["usedPercent", "used_percent", "usagePercent", "usage_percent", "percent"]));
    if (percent === null && used !== null && total !== null && total > 0) percent = used / total * 100;
    if (percent !== null) {
      var reset = parseDate(ctx, first(value, ["nextResetAt", "next_reset_at", "resetAt", "reset_at"]));
      var detail = used !== null && total !== null ? Math.round(used) + "/" + Math.round(total) + " credits" : null;
      var line = {
        label: stringFromKeys(value, ["label", "name", "type"]) || "Credits",
        used: Math.max(0, Math.min(100, percent)),
        limit: 100,
        format: { kind: "percent" },
      };
      if (reset) line.resetsAt = reset;
      if (detail) line.detail = detail;
      out.push(ctx.line.progress(line));
    }
    var keys = Object.keys(value);
    for (var j = 0; j < keys.length; j++) collectCreditWindows(ctx, value[keys[j]], out);
  }

  function snapshotFromPayload(ctx, value, loginMethod) {
    var root = value && value.data && typeof value.data === "object" ? value.data : value;
    var summary = quotaSummarySnapshot(ctx, root, loginMethod);
    if (summary) return summary;
    var lines = [];
    collectCreditWindows(ctx, root, lines);
    lines.sort(function (a, b) { return b.used - a.used; });
    if (!lines.length) throw "Missing Qoder credit usage.";
    return { displayName: "Qoder", source: "web", plan: loginMethod, lines: lines };
  }

  function probe(ctx) {
    var cookie = cookieHeader(ctx);
    if (!cookie) throw "Qoder session not configured. Set QODER_COOKIE or provider cookieHeader.";
    var authFailed = false;
    var lastError = null;
    var regions = [
      [GLOBAL_API, "https://qoder.com/account/usage", "Qoder"],
      [CHINA_API, "https://qoder.com.cn/account/usage", "Qoder China"],
    ];
    for (var i = 0; i < regions.length; i++) {
      try {
        return snapshotFromPayload(ctx, requestRegion(ctx, regions[i][0], regions[i][1], cookie), regions[i][2]);
      } catch (error) {
        if (error && error.auth) authFailed = true;
        else lastError = error;
      }
    }
    if (authFailed) throw "Qoder session expired.";
    throw lastError || "No Qoder usage payload.";
  }

  globalThis.__openusage_plugin = { id: "qoder", probe: probe };
})();
