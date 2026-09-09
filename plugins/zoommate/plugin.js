(function () {
  var API_HOSTS = ["ai.zoom.us", "zoommate.zoom.us"];
  var STATUS_PATH = "/ai-computer/api/v1/credits/status";
  var ORIGIN_REFERER = "https://zoommate.zoom.us";
  var USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

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

  function bearer(value) {
    var raw = trim(value);
    if (!raw) return null;
    return raw.slice(0, 7).toLowerCase() === "bearer " ? raw : "Bearer " + raw;
  }

  function normalizeCookie(raw) {
    var header = trim(raw);
    if (!header) return null;
    if (header.slice(0, 7).toLowerCase() === "cookie:") header = header.slice(7).trim();
    return header.indexOf("=") >= 0 ? header : null;
  }

  function directInput(ctx) {
    return trim(ctx.provider && ctx.provider.cookieHeader) ||
      setting(ctx, ["curl", "curlCapture", "cookieHeader", "cookie", "bearerToken", "token"]) ||
      env(ctx, "ZOOMMATE_BEARER_TOKEN") ||
      env(ctx, "ZOOMMATE_COOKIE");
  }

  function headerMapFromCurl(raw) {
    var headers = {};
    var re = /(?:-H|--header)\s+(['"])([\s\S]*?)\1/g;
    var match;
    while ((match = re.exec(raw))) {
      var idx = match[2].indexOf(":");
      if (idx < 0) continue;
      var name = match[2].slice(0, idx).trim();
      var value = match[2].slice(idx + 1).trim();
      if (name && value) headers[name.toLowerCase()] = value;
    }
    return headers;
  }

  function urlFromCurl(raw) {
    var match = /curl(?:\s+--location)?\s+(['"])(https:\/\/[^'"]+)\1/.exec(raw);
    return match && match[2] ? match[2] : null;
  }

  function hostFromStatusUrl(url) {
    var match = /^https:\/\/(ai\.zoom\.us|zoommate\.zoom\.us)\/ai-computer\/api\/v1\/credits\/status$/.exec(url || "");
    return match ? match[1] : null;
  }

  function contextFromCurl(raw) {
    if (!/^\s*curl\b/i.test(raw)) return null;
    var url = urlFromCurl(raw);
    var host = hostFromStatusUrl(url);
    if (!host) return null;
    var headers = headerMapFromCurl(raw);
    var auth = bearer(headers.authorization);
    if (!auth) return null;
    var cookies = {};
    if (headers.cookie) cookies[host] = headers.cookie;
    return { authorization: auth, cookies: cookies, preferredHost: host, accountEmail: null };
  }

  function headersFor(host, authorization, cookie) {
    var headers = {
      Accept: "application/json, text/plain, */*",
      "Accept-Language": "en-US,en;q=0.9",
      "User-Agent": USER_AGENT,
      "Sec-Fetch-Dest": "empty",
      "Sec-Fetch-Mode": "cors",
      "Sec-Fetch-Site": "same-site",
      Authorization: authorization,
      Origin: ORIGIN_REFERER,
      Referer: ORIGIN_REFERER,
    };
    if (cookie) headers.Cookie = cookie;
    return headers;
  }

  function parseJsonResponse(ctx, resp, label) {
    if (ctx.util.isAuthStatus(resp.status)) throw { auth: true, message: "ZoomMate login required." };
    if (resp.status < 200 || resp.status >= 300) throw "ZoomMate " + label + " error: HTTP " + resp.status + ".";
    var json = ctx.util.tryParseJson(resp.bodyText);
    if (!json || typeof json !== "object") throw { parse: true, message: "ZoomMate " + label + " parse failed." };
    return json;
  }

  function mintBearerOnHost(ctx, cookie, host) {
    var resp = ctx.util.request({
      method: "GET",
      url: "https://" + host + "/ai-computer/api/v1/login/?continue=https://zoommate.zoom.us/",
      headers: {
        Cookie: cookie,
        Accept: "application/json, text/plain, */*",
        "Accept-Language": "en-US,en;q=0.9",
        "User-Agent": USER_AGENT,
        Origin: ORIGIN_REFERER,
        Referer: ORIGIN_REFERER,
        "Sec-Fetch-Dest": "empty",
        "Sec-Fetch-Mode": "cors",
        "Sec-Fetch-Site": "same-site",
      },
      timeoutMs: 15000,
    });
    var json = parseJsonResponse(ctx, resp, "login bootstrap");
    var nak = json.data && trim(json.data.nak);
    if (!nak) throw { parse: true, message: "Missing nak in ZoomMate login bootstrap response." };
    var email = json.data && json.data.user_profile && trim(json.data.user_profile.email);
    var cookies = {};
    cookies[host] = cookie;
    return { authorization: bearer(nak), cookies: cookies, preferredHost: host, accountEmail: email };
  }

  function mintBearer(ctx, cookie) {
    var last = null;
    for (var i = 0; i < API_HOSTS.length; i++) {
      try {
        return mintBearerOnHost(ctx, cookie, API_HOSTS[i]);
      } catch (error) {
        if (error && (error.auth || error.parse)) throw error;
        last = error;
      }
    }
    throw last || "No ZoomMate API host succeeded.";
  }

  function contextFromInput(ctx, raw) {
    var curl = contextFromCurl(raw || "");
    if (curl) return curl;
    var envToken = env(ctx, "ZOOMMATE_BEARER_TOKEN");
    var configuredToken = setting(ctx, ["bearerToken", "token"]);
    var explicitBearer = bearer(configuredToken) || bearer(envToken);
    if (explicitBearer && (raw === configuredToken || raw === envToken)) {
      return { authorization: explicitBearer, cookies: {}, preferredHost: null, accountEmail: null };
    }
    if (raw && (/^bearer\s+\S+$/i.test(raw) || raw.indexOf("=") < 0 && raw.indexOf(" ") < 0)) {
      return { authorization: bearer(raw), cookies: {}, preferredHost: null, accountEmail: null };
    }
    var cookie = normalizeCookie(raw);
    if (!cookie) throw "Paste a ZoomMate cURL capture with Authorization: Bearer, set ZOOMMATE_BEARER_TOKEN, or set ZOOMMATE_COOKIE.";
    return mintBearer(ctx, cookie);
  }

  function hosts(preferred) {
    if (preferred === "ai.zoom.us") return ["ai.zoom.us", "zoommate.zoom.us"];
    if (preferred === "zoommate.zoom.us") return ["zoommate.zoom.us", "ai.zoom.us"];
    return API_HOSTS;
  }

  function cookieFor(context, host) {
    if (!context.cookies) return null;
    return context.cookies[host] || null;
  }

  function fetchStatus(ctx, context) {
    var order = hosts(context.preferredHost);
    var last = null;
    for (var i = 0; i < order.length; i++) {
      var host = order[i];
      try {
        var resp = ctx.util.request({
          method: "GET",
          url: "https://" + host + STATUS_PATH,
          headers: headersFor(host, context.authorization, cookieFor(context, host)),
          timeoutMs: 15000,
        });
        return parseJsonResponse(ctx, resp, "API");
      } catch (error) {
        if (error && (error.auth || error.parse)) throw error;
        last = error;
      }
    }
    throw last || "No ZoomMate API host succeeded.";
  }

  function dateFromMillis(ms) {
    var n = Number(ms);
    var date = new Date(n);
    return Number.isFinite(n) && n > 0 && Number.isFinite(date.getTime()) ? date.toISOString() : null;
  }

  function snapshot(ctx, envelope, email) {
    var status = envelope && envelope.data && envelope.data.credit_status;
    if (!status) throw "Missing credit_status object.";
    var unlimited = status.is_unlimited === true;
    if (unlimited) return { displayName: "ZoomMate", source: "web", lines: [ctx.line.badge({ label: "Credits", text: "Unlimited" })] };
    var budget = status.budget_cap;
    var used = status.used_credit;
    if (typeof budget !== "number" || !Number.isFinite(budget) || budget <= 0 || typeof used !== "number" || !Number.isFinite(used) || used < 0) throw "ZoomMate returned invalid credit values.";
    var percent = Math.max(0, Math.min(100, used / budget * 100));
    var start = Number(status.cycle_start_date) || 0;
    var end = Number(status.cycle_end_date) || 0;
    var opts = {
      label: "Credits",
      used: percent,
      limit: 100,
      format: { kind: "percent" },
      detail: unlimited ? "Unlimited credits" : "Credits",
    };
    if (!unlimited && budget > 0) {
      var reset = dateFromMillis(end);
      if (reset) opts.resetsAt = reset;
      if (end > start) opts.periodDurationMs = Math.max(1, Math.round((end - start) / 60000)) * 60 * 1000;
    }
    var lines = [ctx.line.progress(opts)];
    if (email) lines.push(ctx.line.text({ label: "Account", value: email }));
    return { displayName: "ZoomMate", source: "web", plan: email ? "Cookie" : null, lines: lines };
  }

  function probe(ctx) {
    var raw = directInput(ctx);
    if (!raw) throw "ZoomMate session not configured. Paste cURL, set ZOOMMATE_BEARER_TOKEN, or set ZOOMMATE_COOKIE.";
    var requestContext = contextFromInput(ctx, raw);
    try {
      return snapshot(ctx, fetchStatus(ctx, requestContext), requestContext.accountEmail);
    } catch (error) {
      if (error && error.message) throw error.message;
      throw error;
    }
  }

  globalThis.__openusage_plugin = { id: "zoommate", probe: probe };
})();
