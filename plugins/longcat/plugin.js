(function () {
  var HOST = "https://longcat.chat";
  var USER_CURRENT = "/api/v1/user-current";
  var TOKEN_USAGE = "/api/lc-platform/v1/tokenUsage";
  var PENDING_FUEL = "/api/lc-platform/v1/pending-fuel-packages";
  var TOKEN_PACKS_SUMMARY = "/api/pay/quota/metering/token-packs/summary";

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
    return header.indexOf("=") >= 0 ? header : null;
  }

  function cookieHeader(ctx) {
    return normalizeCookie(ctx.provider && ctx.provider.cookieHeader) ||
      normalizeCookie(setting(ctx, ["cookieHeader", "cookie"])) ||
      normalizeCookie(env(ctx, "LONGCAT_COOKIE"));
  }

  function requestJson(ctx, method, path, cookie, optional) {
    var result = ctx.util.requestJson({
      method: method,
      url: HOST + path,
      headers: {
        Cookie: cookie,
        Accept: "application/json",
        "Content-Type": method === "POST" ? "application/json" : undefined,
      },
      bodyText: method === "POST" ? "{}" : undefined,
      timeoutMs: optional ? 2000 : 10000,
    });
    if (ctx.util.isAuthStatus(result.resp.status)) throw "LongCat session expired.";
    if (result.resp.status < 200 || result.resp.status >= 300) {
      if (optional) return null;
      throw "LongCat API " + path + " returned HTTP " + result.resp.status + ".";
    }
    if (!result.json || typeof result.json !== "object") {
      if (optional) return null;
      throw "Failed to parse LongCat " + path + ".";
    }
    var code = result.json.code != null ? Number(result.json.code) : result.json.status != null ? Number(result.json.status) : null;
    if (code === 401 || code === 403) throw "LongCat session expired.";
    return result.json;
  }

  function data(value) {
    return value && value.data && typeof value.data === "object" ? value.data : value;
  }

  function number(obj, key) {
    if (!obj || typeof obj !== "object") return null;
    var value = obj[key];
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function string(obj, key) {
    var value = obj && obj[key];
    return typeof value === "string" && value.trim() ? value.trim() : null;
  }

  function activeLot(summary) {
    var lot = data(summary);
    lot = lot && lot.currentLot;
    if (!lot || typeof lot !== "object") return null;
    if (String(string(lot, "status") || "").toUpperCase() !== "ACTIVE") return null;
    var total = number(lot, "totalToken");
    return total && total > 0 ? lot : null;
  }

  function parseFuel(ctx, fuel) {
    var root = data(fuel);
    var packages = root && Array.isArray(root.packages) ? root.packages : Array.isArray(root) ? root : null;
    if (!packages) return null;
    var total = 0;
    var remaining = 0;
    var sawRemaining = false;
    var nearest = null;
    for (var i = 0; i < packages.length; i++) {
      var pkg = packages[i] || {};
      var capacity = number(pkg, "totalToken");
      if (capacity === null) capacity = number(pkg, "total");
      if (capacity === null) capacity = number(pkg, "amount");
      var rem = number(pkg, "availableToken");
      if (rem === null) rem = number(pkg, "remainingToken");
      if (rem === null) rem = number(pkg, "remaining");
      if (capacity === null || capacity < 0 || rem === null || rem < 0) return null;
      total += capacity;
      remaining += rem;
      sawRemaining = true;
      var raw = string(pkg, "expireTime") || string(pkg, "expireAt") || string(pkg, "expiresAt");
      var iso = raw ? ctx.util.toIso(raw) : null;
      if (iso && (!nearest || iso < nearest)) nearest = iso;
    }
    if (total <= 0 && !sawRemaining) return null;
    if (total <= 0) total = remaining;
    if (!sawRemaining) remaining = total;
    return { total: total, remaining: remaining, resetsAt: nearest };
  }

  function progress(ctx, label, used, total, detail, reset) {
    var opts = {
      label: label,
      used: total > 0 ? Math.max(0, Math.min(100, used / total * 100)) : 0,
      limit: 100,
      format: { kind: "percent" },
      detail: detail,
    };
    if (reset) opts.resetsAt = reset;
    return ctx.line.progress(opts);
  }

  function accountName(account) {
    var root = data(account);
    return string(root, "name") || string(root, "nickname") || string(root, "userName");
  }

  function probe(ctx) {
    var cookie = cookieHeader(ctx);
    if (!cookie) throw "LongCat session not configured. Set LONGCAT_COOKIE or provider cookieHeader.";
    var account = requestJson(ctx, "GET", USER_CURRENT, cookie, false);
    var tokenPacks = requestJson(ctx, "POST", TOKEN_PACKS_SUMMARY, cookie, true);
    var lot = tokenPacks ? activeLot(tokenPacks) : null;
    var total;
    var used;
    var usage;
    if (lot) {
      total = number(lot, "totalToken");
      used = number(lot, "consumedToken");
      if (used === null || used < 0) throw "LongCat token pack was missing consumedToken.";
    } else {
      usage = data(requestJson(ctx, "GET", TOKEN_USAGE, cookie, false));
      var usageObj = usage && usage.usage && typeof usage.usage === "object" ? usage.usage : usage;
      total = number(usageObj, "totalToken");
      var remaining = number(usageObj, "availableToken");
      if (total === null || total <= 0 || remaining === null || remaining < 0) throw "LongCat tokenUsage data was missing usable totalToken or availableToken.";
      used = Math.max(0, total - remaining);
    }
    var lines = [progress(ctx, "Quota", used, total, Math.round(used) + "/" + Math.round(total) + " tokens", null)];
    var fuel = requestJson(ctx, "GET", PENDING_FUEL, cookie, true);
    var fuelTotals = fuel ? parseFuel(ctx, fuel) : null;
    if (fuelTotals && fuelTotals.total > 0) {
      lines.push(progress(
        ctx,
        "Fuel Pack",
        Math.max(0, fuelTotals.total - fuelTotals.remaining),
        fuelTotals.total,
        "Fuel pack: " + Math.round(fuelTotals.remaining) + "/" + Math.round(fuelTotals.total),
        fuelTotals.resetsAt
      ));
    }
    return { displayName: "LongCat", source: "web", plan: accountName(account), lines: lines };
  }

  globalThis.__openusage_plugin = { id: "longcat", probe: probe };
})();
