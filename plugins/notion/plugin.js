(function () {
  var BASE_URL = "https://app.notion.com";
  var GET_SPACES_URL = "https://app.notion.com/api/v3/getSpaces";
  var RATE_LIMIT_URL = "https://app.notion.com/api/v3/getCreditRateLimitStatus";
  var SESSION_COOKIE_NAME = "token_v2";
  var USER_AGENT = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

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
    var value = trim(raw);
    if (!value) return null;
    if (value.slice(0, 7).toLowerCase() === "cookie:") value = value.slice(7).trim();
    if (!value) return null;
    if (value.indexOf("=") < 0) return SESSION_COOKIE_NAME + "=" + value;
    var pairs = [];
    var chunks = value.split(";");
    for (var i = 0; i < chunks.length; i++) {
      var idx = chunks[i].indexOf("=");
      if (idx < 0) continue;
      var name = chunks[i].slice(0, idx).trim();
      var val = chunks[i].slice(idx + 1).trim();
      if (name && val) pairs.push(name + "=" + val);
    }
    return pairs.length ? pairs.join("; ") : null;
  }

  function cookieHeader(ctx) {
    return normalizeCookie(ctx.provider && ctx.provider.cookieHeader) ||
      normalizeCookie(setting(ctx, ["cookieHeader", "cookie", "token"])) ||
      normalizeCookie(env(ctx, "NOTION_COOKIE"));
  }

  function spaceOverride(ctx) {
    return setting(ctx, ["spaceId", "workspaceId", "workspace"]) ||
      trim(ctx.provider && ctx.provider.workspaceId) ||
      env(ctx, "NOTION_SPACE_ID");
  }

  function requestJson(ctx, url, cookie, body) {
    var result = ctx.util.requestJson({
      method: "POST",
      url: url,
      headers: {
        Cookie: cookie,
        "Content-Type": "application/json",
        Accept: "*/*",
        "Accept-Language": "en-US,en;q=0.9",
        "User-Agent": USER_AGENT,
        Referer: BASE_URL + "/",
        Origin: BASE_URL,
        "Sec-Fetch-Dest": "empty",
        "Sec-Fetch-Mode": "cors",
        "Sec-Fetch-Site": "same-origin",
      },
      bodyText: JSON.stringify(body || {}),
      timeoutMs: 15000,
    });
    if (result.resp.status === 401) throw "Notion login required.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "Notion API error: HTTP " + result.resp.status + ".";
    if (!result.json || typeof result.json !== "object") throw "Could not parse Notion usage.";
    return result.json;
  }

  function unwrapRecord(raw) {
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
    var value = raw.value && typeof raw.value === "object" ? raw.value : raw;
    if (value.value && typeof value.value === "object" && !Array.isArray(value.value)) return value.value;
    return value;
  }

  function resolveUserId(root) {
    var ids = [];
    var keys = Object.keys(root || {});
    for (var i = 0; i < keys.length; i++) {
      var key = keys[i];
      var container = root[key];
      var users = container && container.notion_user;
      var record = users && unwrapRecord(users[key]);
      if (record && record.id === key) ids.push(key);
    }
    if (ids.length === 1) return ids[0];
    return keys.length === 1 ? keys[0] : null;
  }

  function normalizeSpaceId(raw) {
    var text = trim(raw);
    if (!text) return null;
    var compact = text.replace(/-/g, "").toLowerCase();
    if (/^[0-9a-f]{32}$/.test(compact)) {
      return compact.slice(0, 8) + "-" + compact.slice(8, 12) + "-" + compact.slice(12, 16) + "-" +
        compact.slice(16, 20) + "-" + compact.slice(20);
    }
    return text.toLowerCase();
  }

  function parseSpaces(json) {
    var root = json && typeof json === "object" && !Array.isArray(json) ? json : null;
    if (!root) throw "getSpaces response is not a JSON object.";
    var userId = resolveUserId(root);
    if (!userId || !root[userId]) throw "getSpaces response did not identify a single user.";
    var container = root[userId];
    var email = null;
    var users = container.notion_user || {};
    var record = unwrapRecord(users[userId]);
    if (!record) {
      var userKeys = Object.keys(users);
      for (var i = 0; i < userKeys.length && !record; i++) record = unwrapRecord(users[userKeys[i]]);
    }
    if (record && typeof record.email === "string") email = record.email;

    var spaces = [];
    var spaceObj = container.space || {};
    var spaceKeys = Object.keys(spaceObj).sort();
    for (var j = 0; j < spaceKeys.length; j++) {
      var key = spaceKeys[j];
      var ws = unwrapRecord(spaceObj[key]);
      if (!ws) continue;
      spaces.push({
        id: String(ws.id || key),
        name: trim(ws.name),
        tier: trim(ws.subscription_tier),
      });
    }
    return { email: email, workspaces: spaces };
  }

  function mayHaveAllowance(workspace) {
    var tier = String(workspace && workspace.tier || "").toLowerCase();
    return tier === "business" || tier === "enterprise";
  }

  function pickWorkspace(account, preferredId) {
    var preferred = normalizeSpaceId(preferredId);
    if (preferred) {
      for (var i = 0; i < account.workspaces.length; i++) {
        if (normalizeSpaceId(account.workspaces[i].id) === preferred) return account.workspaces[i];
      }
      throw "Selected Notion workspace was not found in this account.";
    }
    for (var j = 0; j < account.workspaces.length; j++) {
      if (mayHaveAllowance(account.workspaces[j])) return account.workspaces[j];
    }
    return account.workspaces[0] || null;
  }

  function title(value) {
    var text = trim(value);
    return text ? text.charAt(0).toUpperCase() + text.slice(1) : null;
  }

  function number(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function percent(used, limit) {
    used = number(used);
    limit = number(limit);
    if (used === null || limit === null || limit <= 0) return null;
    return Math.max(0, used / limit * 100);
  }

  function minutesFromWindow(raw) {
    var text = String(raw || "").trim().toLowerCase();
    var match = /^(\d+)([mhdw])$/.exec(text);
    if (!match) return null;
    var n = Number(match[1]);
    if (!n) return null;
    if (match[2] === "m") return n;
    if (match[2] === "h") return n * 60;
    if (match[2] === "d") return n * 24 * 60;
    return n * 7 * 24 * 60;
  }

  function rollingReset(ctx, seconds) {
    var n = number(seconds);
    if (n === null || n < 0) return null;
    return ctx.util.toIso(Date.parse(ctx.nowIso) + n * 1000);
  }

  function dateFromMs(value) {
    var n = number(value);
    if (n === null || n <= 0) return null;
    var date = new Date(n);
    return Number.isFinite(date.getTime()) ? date.toISOString() : null;
  }

  function parseRateLimit(json) {
    return {
      status: trim(json && json.status),
      window: json && json.window && typeof json.window === "object" ? json.window : null,
      billing: json && json.billingPeriodWindow && typeof json.billingPeriodWindow === "object" ? json.billingPeriodWindow : null,
      resetsInSeconds: number(json && json.resetsInSeconds),
    };
  }

  function buildSnapshot(ctx, rateLimit, workspace, account) {
    if (rateLimit.status && rateLimit.status.toLowerCase() === "not_applicable") {
      throw "Notion AI usage allowance is not tracked for " + (workspace.name || "this workspace") + ".";
    }
    var lines = [];
    if (rateLimit.window) {
      var pct = percent(rateLimit.window.used, rateLimit.window.limit);
      if (pct !== null) {
        var minutes = minutesFromWindow(rateLimit.window.window);
        var opts = {
          label: "Rolling",
          used: Math.max(0, Math.min(100, pct)),
          limit: 100,
          format: { kind: "percent" },
        };
        if (minutes && minutes !== 30 * 24 * 60) opts.periodDurationMs = minutes * 60 * 1000;
        var rolling = rollingReset(ctx, rateLimit.resetsInSeconds);
        if (rolling) opts.resetsAt = rolling;
        lines.push(ctx.line.progress(opts));
      }
    }
    if (rateLimit.billing) {
      var bpct = percent(rateLimit.billing.used, rateLimit.billing.limit);
      if (bpct !== null) {
        var bopts = {
          label: "Monthly",
          used: Math.max(0, Math.min(100, bpct)),
          limit: 100,
          format: { kind: "percent" },
        };
        var reset = dateFromMs(rateLimit.billing.periodEndMs);
        if (reset) {
          bopts.resetsAt = reset;
          bopts.periodDurationMs = ctx.util.calendarMonthDuration(reset);
        }
        var start = dateFromMs(rateLimit.billing.periodStartMs);
        if (reset && start && Date.parse(reset) > Date.parse(start)) bopts.periodDurationMs = Date.parse(reset) - Date.parse(start);
        lines.push(ctx.line.progress(bopts));
      }
    }
    if (!lines.length) throw "getCreditRateLimitStatus returned no measurable usage windows.";
    if (account.email) lines.push(ctx.line.text({ label: "Account", value: account.email }));
    return {
      displayName: "Notion AI",
      source: "web",
      plan: title(workspace.tier),
      lines: lines,
    };
  }

  function probe(ctx) {
    var cookie = cookieHeader(ctx);
    if (!cookie) throw "Notion session not configured. Set NOTION_COOKIE or provider cookieHeader.";
    var account = parseSpaces(requestJson(ctx, GET_SPACES_URL, cookie, {}));
    var workspace = pickWorkspace(account, spaceOverride(ctx));
    if (!workspace) throw "No Notion workspace found for this account.";
    var rateLimit = parseRateLimit(requestJson(ctx, RATE_LIMIT_URL, cookie, { spaceId: workspace.id }));
    return buildSnapshot(ctx, rateLimit, workspace, account);
  }

  globalThis.__openusage_plugin = { id: "notion", probe: probe };
})();
