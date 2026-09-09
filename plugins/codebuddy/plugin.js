(function () {
  var API_URL = "https://www.codebuddy.cn/billing/meter/get-user-resource";
  var ORIGIN = "https://www.codebuddy.cn";
  var REFERER = "https://www.codebuddy.cn/profile/plans-usage";
  var PRODUCT_CODE = "p_tcaca";
  var USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";
  var MAX_RESPONSE_BYTES = 8 * 1024 * 1024;
  var DEFAULT_PACKAGE_CODES = [
    "TCACA_code_007_nzdH5h4Nl0",
    "TCACA_code_029_6wCGEWquYy",
    "TCACA_code_030_BjSt89qTvr",
    "TCACA_code_008_cfWoLwvjU4",
    "TCACA_code_002_AkiJS3ZHF5",
    "TCACA_code_023_4xbGhMrE6q",
    "TCACA_code_026_BaESVICNoi",
    "TCACA_code_027_0FCGVA6vSa",
  ];

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
    header = header.replace(/^\uFEFF/, "").replace(/\^/g, "").trim();
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

  function codebuddyHome(ctx) {
    return env(ctx, "CODEBUDDY_HOME") || (ctx.host.fs && ctx.host.fs.homeDir ? ctx.host.fs.homeDir + "/.codebuddy" : null);
  }

  function cookieFilePath(ctx) {
    return env(ctx, "CB_COOKIE_FILE") || (codebuddyHome(ctx) ? codebuddyHome(ctx) + "/cb_cookie.txt" : null);
  }

  function creditsCachePath(ctx) {
    return env(ctx, "CB_CREDITS_FILE") || (codebuddyHome(ctx) ? codebuddyHome(ctx) + "/cb_credits.json" : null);
  }

  function readTextIfExists(ctx, path) {
    if (!path || !ctx.host.fs || !ctx.host.fs.exists(path)) return null;
    return ctx.host.fs.readText(path);
  }

  function cookieHeader(ctx) {
    var direct = normalizeCookie(ctx.provider && ctx.provider.cookieHeader) ||
      normalizeCookie(setting(ctx, ["cookieHeader", "cookie"])) ||
      normalizeCookie(env(ctx, "CODEBUDDY_COOKIE")) ||
      normalizeCookie(env(ctx, "CB_COOKIE"));
    if (direct) return direct;
    return normalizeCookie(readTextIfExists(ctx, cookieFilePath(ctx)));
  }

  function apiUrl(ctx) {
    var raw = setting(ctx, ["apiUrl", "apiBase"]) || env(ctx, "CB_API_URL") || API_URL;
    return ctx.host.http.validateBaseUrl(raw, true);
  }

  function packageCodes(ctx) {
    var raw = env(ctx, "CB_PACKAGE_CODES");
    if (raw) {
      try {
        var parsed = JSON.parse(raw);
        if (Array.isArray(parsed)) {
          var out = [];
          for (var i = 0; i < parsed.length && out.length < 64; i++) {
            var code = trim(parsed[i]);
            if (code && code.length <= 128 && !/[\x00-\x1f\x7f]/.test(code)) out.push(code);
          }
          if (out.length) return out;
        }
      } catch (_) {}
      ctx.host.log.warn("CodeBuddy: CB_PACKAGE_CODES had no usable entries; using defaults");
    }
    return DEFAULT_PACKAGE_CODES;
  }

  function requestBody(ctx) {
    return {
      PageNumber: 1,
      PageSize: 200,
      ProductCode: PRODUCT_CODE,
      Status: [0, 3],
      OnlyValidPeriod: true,
      PackageCodes: packageCodes(ctx),
    };
  }

  function number(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function valueFromKeys(obj, precise, fallback) {
    var a = number(obj && obj[precise]);
    return a !== null ? a : number(obj && obj[fallback]);
  }

  function parseDate(ctx, value) {
    if (value === null || value === undefined || value === "") return null;
    return ctx.util.toIso(value);
  }

  function expireTime(ctx, account) {
    var keys = ["ExpireTime", "expireTime", "ExpireTimeStamp", "EndTime", "endTime", "ValidEndTime"];
    for (var i = 0; i < keys.length; i++) {
      var iso = parseDate(ctx, account && account[keys[i]]);
      if (iso) return iso;
    }
    return null;
  }

  function normalizedTotals(total, used, remaining, reset) {
    if (!Number.isFinite(total) || !Number.isFinite(used) || !Number.isFinite(remaining)) {
      throw "CodeBuddy payload contains non-finite credit values.";
    }
    if (remaining > 0 && total > 0 && used <= 0) used = Math.max(0, total - remaining);
    return {
      total: Math.max(0, total),
      used: Math.max(0, used),
      remaining: remaining < 0 ? Math.max(0, total - used) : remaining,
      resetsAt: reset,
    };
  }

  function totalsFromPayload(ctx, value) {
    var code = number(value && value.code);
    if (code !== 0) {
      var msg = String(value && value.msg || "unknown error");
      if (/auth/i.test(msg) || msg.indexOf("登录") >= 0 || msg.indexOf("未登录") >= 0) throw { auth: true, message: "CodeBuddy login required." };
      throw "CodeBuddy API code=" + code + ": " + msg;
    }
    var accounts = value && value.data && value.data.Response && value.data.Response.Data && value.data.Response.Data.Accounts;
    if (!Array.isArray(accounts)) throw "CodeBuddy response missing data.Response.Data.Accounts.";
    if (!accounts.length) throw "CodeBuddy returned zero packages. Set CB_PACKAGE_CODES from the browser cURL body.";
    var total = 0;
    var used = 0;
    var remaining = 0;
    var reset = null;
    for (var i = 0; i < accounts.length; i++) {
      var account = accounts[i] || {};
      var capacity = valueFromKeys(account, "CapacitySizePrecise", "CapacitySize");
      var consumed = valueFromKeys(account, "CapacityUsedPrecise", "CapacityUsed");
      var available = valueFromKeys(account, "CapacityRemainPrecise", "CapacityRemain");
      if (capacity === null || capacity < 0 || (consumed === null && available === null)) throw "CodeBuddy package is missing credit values.";
      if (consumed === null) consumed = Math.max(0, capacity - available);
      if (available === null) available = Math.max(0, capacity - consumed);
      total += capacity;
      used += consumed;
      remaining += available;
      var expiry = expireTime(ctx, account);
      if (expiry && (!reset || expiry < reset)) reset = expiry;
    }
    return normalizedTotals(total, used, remaining, reset);
  }

  function totalsFromCache(ctx, cookie) {
    var path = creditsCachePath(ctx);
    var raw = readTextIfExists(ctx, path);
    if (!raw) throw "No CodeBuddy cookie and no local cache at " + path + ".";
    if (raw.length > 1024 * 1024) throw "CodeBuddy cache at " + path + " is too large.";
    var value = ctx.util.tryParseJson(raw);
    if (!value || typeof value !== "object") throw "Invalid CodeBuddy cache.";
    if (cookie && value.accountHash !== ctx.host.crypto.sha256(cookie + "\n" + apiUrl(ctx))) throw "CodeBuddy cache belongs to different credentials.";
    var updated = parseDate(ctx, value.updatedAt);
    if (cookie && (!updated || Date.parse(ctx.nowIso) - Date.parse(updated) > 24 * 3600000 || Date.parse(updated) > Date.parse(ctx.nowIso))) throw "CodeBuddy cache is too old.";
    var total = number(value.total);
    if (total === null || total < 0) throw "cb_credits.json invalid total.";
    var used = number(value.used);
    var remaining = number(value.remaining);
    if (used === null && remaining === null) throw "CodeBuddy cache is missing credit values.";
    if (used === null) used = Math.max(0, total - remaining);
    if (remaining === null) remaining = Math.max(0, total - used);
    var totals = normalizedTotals(total, used, remaining, parseDate(ctx, value.resetsAt));
    totals.fetchedAt = updated;
    return totals;
  }

  function writeCache(ctx, totals, cookie) {
    var path = creditsCachePath(ctx);
    if (!path || !ctx.host.fs || typeof ctx.host.fs.writeText !== "function") return;
    var value = {
      total: totals.total,
      used: totals.used,
      remaining: totals.remaining,
      source: "api",
      updatedAt: ctx.nowIso,
      accountHash: ctx.host.crypto.sha256(cookie + "\n" + apiUrl(ctx)),
    };
    if (totals.resetsAt) value.resetsAt = totals.resetsAt;
    try {
      ctx.host.fs.writeText(path, JSON.stringify(value, null, 2));
    } catch (error) {
      ctx.host.log.warn("CodeBuddy: failed to write local credits cache: " + String(error));
    }
  }

  function looksLikeHtml(text) {
    return /^\s*</.test(String(text || ""));
  }

  function fetchOnce(ctx, cookie) {
    var resp = ctx.util.request({
      method: "POST",
      url: apiUrl(ctx),
      headers: {
        Cookie: cookie,
        Accept: "application/json, text/plain, */*",
        "Accept-Language": "zh-CN,zh;q=0.9,en;q=0.7",
        "Content-Type": "application/json",
        Origin: ORIGIN,
        Referer: REFERER,
        "User-Agent": USER_AGENT,
        "x-client-platform": "web",
        "sec-fetch-dest": "empty",
        "sec-fetch-mode": "cors",
        "sec-fetch-site": "same-origin",
      },
      bodyText: JSON.stringify(requestBody(ctx)),
      timeoutMs: 20000,
    });
    if (ctx.util.isAuthStatus(resp.status)) throw { auth: true, message: "CodeBuddy login required." };
    if (resp.status === 429 || resp.status >= 500) throw { transient: true, message: "CodeBuddy temporary HTTP " + resp.status + "." };
    if (resp.status < 200 || resp.status >= 300) throw "CodeBuddy get-user-resource returned HTTP " + resp.status + ".";
    if ((resp.bodyText || "").length > MAX_RESPONSE_BYTES) throw "CodeBuddy response exceeds " + MAX_RESPONSE_BYTES + " bytes.";
    if (looksLikeHtml(resp.bodyText)) throw { transient: true, message: "CodeBuddy WAF/HTML response." };
    var json = ctx.util.tryParseJson(resp.bodyText);
    if (!json) throw { transient: true, message: "Failed to parse CodeBuddy response." };
    return totalsFromPayload(ctx, json);
  }

  function fetchWeb(ctx, cookie) {
    var last = null;
    for (var attempt = 0; attempt < 2; attempt++) {
      try {
        var totals = fetchOnce(ctx, cookie);
        writeCache(ctx, totals, cookie);
        return { totals: totals, source: "web" };
      } catch (error) {
        if (error && error.transient && attempt === 0) {
          last = error;
          continue;
        }
        throw error;
      }
    }
    throw last || "CodeBuddy fetch failed.";
  }

  function formatNumber(value) {
    var rounded = Math.round(value * 100) / 100;
    var text = Math.abs(rounded - Math.round(rounded)) < 0.001 ? String(Math.round(rounded)) : rounded.toFixed(2).replace(/\.?0+$/, "");
    return text.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  }

  function resultFromTotals(ctx, totals, source) {
    var percent = totals.total > 0 ? totals.used / totals.total * 100 : totals.used > 0 ? 100 : 0;
    var detail = formatNumber(totals.remaining) + " / " + formatNumber(totals.total) + " left";
    var opts = {
      label: "Credits",
      used: Math.max(0, Math.min(100, percent)),
      limit: 100,
      format: { kind: "percent" },
      detail: detail,
    };
    if (totals.resetsAt) opts.resetsAt = totals.resetsAt;
    return { displayName: "CodeBuddy", source: source, fetchedAt: totals.fetchedAt || ctx.nowIso, plan: "CodeBuddy CN", lines: [ctx.line.progress(opts)] };
  }

  function probe(ctx) {
    if (ctx.sourceMode === "local" || ctx.sourceMode === "cli") {
      return resultFromTotals(ctx, totalsFromCache(ctx), "local");
    }
    var cookie = cookieHeader(ctx);
    if (!cookie) {
      try {
        return resultFromTotals(ctx, totalsFromCache(ctx), "local");
      } catch (_) {
        throw "CodeBuddy session not configured. Set CODEBUDDY_COOKIE, CB_COOKIE, provider cookieHeader, or ~/.codebuddy/cb_cookie.txt.";
      }
    }
    try {
      var fetched = fetchWeb(ctx, cookie);
      return resultFromTotals(ctx, fetched.totals, fetched.source);
    } catch (error) {
      if (error && error.auth) throw error.message;
      if (error && error.transient) {
        try {
          return resultFromTotals(ctx, totalsFromCache(ctx, cookie), "cached");
        } catch (_) {}
        throw error.message;
      }
      throw error;
    }
  }

  globalThis.__openusage_plugin = { id: "codebuddy", probe: probe };
})();
