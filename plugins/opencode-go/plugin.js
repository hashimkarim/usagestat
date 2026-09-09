(function () {
  const PROVIDER_ID = "opencode-go";
  const BASE_URL = "https://opencode.ai";
  const SERVER_URL = "https://opencode.ai/_server";
  const WORKSPACES_SERVER_ID =
    "def39973159c7f0483d8793a822b8dbb10d067e12c65455fcb4608459ba0234f";
  const FIVE_HOURS_MS = 5 * 60 * 60 * 1000;
  const WEEK_MS = 7 * 24 * 60 * 60 * 1000;
  const LIMITS = {
    session: 12,
    weekly: 30,
    monthly: 60,
  };

  const HISTORY_EXISTS_SQL = `
    SELECT 1 AS present
    FROM message
    WHERE json_valid(data)
      AND json_extract(data, '$.providerID') = 'opencode-go'
      AND json_extract(data, '$.role') = 'assistant'
      AND json_type(data, '$.cost') IN ('integer', 'real')
    LIMIT 1
  `;

  const HISTORY_ROWS_SQL = `
    SELECT
      CAST(COALESCE(json_extract(data, '$.time.created'), time_created) AS INTEGER) AS createdMs,
      CAST(json_extract(data, '$.cost') AS REAL) AS cost
    FROM message
    WHERE json_valid(data)
      AND json_extract(data, '$.providerID') = 'opencode-go'
      AND json_extract(data, '$.role') = 'assistant'
      AND json_type(data, '$.cost') IN ('integer', 'real')
  `;

  function readNumber(value) {
    if (typeof value !== "number" && typeof value !== "string") return null;
    if (typeof value === "string" && !value.trim()) return null;
    const n = Number(value);
    return Number.isFinite(n) ? n : null;
  }

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
    const settings = ctx.provider && ctx.provider.settings ? ctx.provider.settings : {};
    for (let i = 0; i < names.length; i += 1) {
      const value = trim(settings[names[i]]);
      if (value) return value;
    }
    return null;
  }

  function pathValue(ctx, name, fromEnv) {
    const value = fromEnv ? ctx.host.env.get(name) : (ctx.provider.settings || {})[name];
    if (value === undefined || value === null || value === "") return null;
    if (typeof value !== "string" || value.indexOf("\u0000") >= 0) {
      throw {code: "failed", message: "OpenCode " + name + " must be a path string."};
    }
    return value;
  }

  function absolute(ctx, value) {
    return ctx.app.platform === "windows" ? /^(?:[A-Za-z]:[\\/]|[\\/]{2}[^\\/]+[\\/][^\\/]+)/.test(value) : value[0] === "/";
  }

  function dataPath(ctx, name) {
    // Upstream uses xdg-basedir on Windows and macOS too, not native AppData.
    const selected = pathValue(ctx, "dataDir", false);
    const xdg = selected ? null : pathValue(ctx, "XDG_DATA_HOME", true);
    const root = selected || (xdg ? xdg.replace(/[\\/]+$/, "") + "/opencode" : ctx.host.fs.homeDir + "/.local/share/opencode");
    if (!absolute(ctx, root)) throw {code: "failed", message: "OpenCode dataDir/XDG_DATA_HOME must be absolute."};
    return root.replace(/[\\/]+$/, "") + "/" + name;
  }

  function databasePath(ctx) {
    const selected = pathValue(ctx, "databasePath", false);
    if (selected) {
      if (!absolute(ctx, selected)) throw {code: "failed", message: "OpenCode databasePath must be absolute."};
      return selected;
    }
    const upstream = pathValue(ctx, "OPENCODE_DB", true);
    if (upstream === ":memory:") throw {code: "unsupported", message: "OpenCode in-memory history cannot be read by another process. Select a persisted databasePath or use web credentials."};
    return upstream && absolute(ctx, upstream) ? upstream : dataPath(ctx, upstream || "opencode.db");
  }

  function cookieHeader(ctx) {
    const cookie = trim(ctx.provider && ctx.provider.cookieHeader) ||
      setting(ctx, ["cookieHeader", "cookie"]) ||
      env(ctx, "OPENCODE_GO_COOKIE") ||
      env(ctx, "OPENCODE_COOKIE");
    return cookie ? cookie.replace(/^Cookie:\s*/i, "").trim() : null;
  }

  function workspaceOverride(ctx) {
    return normalizeWorkspaceId(
      setting(ctx, ["workspaceId", "workspace"]) ||
      (ctx.provider && trim(ctx.provider.workspaceId)) ||
      env(ctx, "OPENCODE_GO_WORKSPACE_ID") ||
      env(ctx, "OPENCODE_WORKSPACE_ID")
    );
  }

  function normalizeWorkspaceId(raw) {
    const text = trim(raw);
    if (!text) return null;
    const match = /wrk_[A-Za-z0-9_-]+/.exec(text);
    return match ? match[0] : null;
  }

  function readNowMs() {
    return Date.now();
  }

  function clampPercent(used, limit) {
    if (!Number.isFinite(used) || !Number.isFinite(limit) || limit <= 0)
      return 0;
    const percent = (used / limit) * 100;
    if (!Number.isFinite(percent)) return 0;
    return Math.round(Math.max(0, Math.min(100, percent)) * 10) / 10;
  }

  function toIso(ms) {
    if (!Number.isFinite(ms)) return null;
    return new Date(ms).toISOString();
  }

  function serverInstance() {
    return "server-fn:" + Math.random().toString(16).slice(2) + Date.now().toString(16);
  }

  function looksSignedOut(text) {
    const lower = String(text || "").toLowerCase();
    return lower.indexOf("auth/authorize") >= 0 ||
      lower.indexOf("\"signin\"") >= 0 ||
      lower.indexOf("please sign in") >= 0 ||
      lower.indexOf("sign in") >= 0;
  }

  function serverRequest(ctx, cookie) {
    const url = SERVER_URL + "?id=" + encodeURIComponent(WORKSPACES_SERVER_ID);
    const resp = ctx.util.request({
      method: "GET",
      url,
      headers: {
        Cookie: cookie,
        "X-Server-Id": WORKSPACES_SERVER_ID,
        "X-Server-Instance": serverInstance(),
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/124.0.0.0 Safari/537.36",
        Origin: BASE_URL,
        Referer: BASE_URL,
        Accept: "text/javascript, application/json;q=0.9, */*;q=0.8",
      },
      timeoutMs: 15000,
    });
    const text = String(resp.bodyText || "");
    if (ctx.util.isAuthStatus(resp.status) || looksSignedOut(text)) {
      throw "OpenCode Go session cookie is invalid or expired.";
    }
    if (resp.status < 200 || resp.status >= 300) {
      throw "OpenCode Go workspace request failed (HTTP " + resp.status + ").";
    }
    return text;
  }

  function parseWorkspaceIds(text) {
    const ids = [];
    const regex = /(wrk_[A-Za-z0-9_-]+)/g;
    let match;
    while ((match = regex.exec(String(text || ""))) !== null) {
      if (ids.indexOf(match[1]) < 0) ids.push(match[1]);
    }
    return ids;
  }

  function fetchWorkspaceId(ctx, cookie) {
    const ids = parseWorkspaceIds(serverRequest(ctx, cookie));
    if (ids.length) return ids[0];
    throw "OpenCode Go parse error: missing workspace id.";
  }

  function fetchUsagePage(ctx, workspaceId, cookie) {
    const url = BASE_URL + "/workspace/" + workspaceId + "/go";
    const resp = ctx.util.request({
      method: "GET",
      url,
      headers: {
        Cookie: cookie,
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/124.0.0.0 Safari/537.36",
        Referer: BASE_URL,
        Accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
      },
      timeoutMs: 15000,
    });
    const text = String(resp.bodyText || "");
    if (ctx.util.isAuthStatus(resp.status) || looksSignedOut(text)) {
      throw "OpenCode Go session cookie is invalid or expired.";
    }
    if (resp.status < 200 || resp.status >= 300) {
      throw "OpenCode Go usage page failed (HTTP " + resp.status + ").";
    }
    return text;
  }

  function startOfUtcWeek(nowMs) {
    const date = new Date(nowMs);
    const offset = (date.getUTCDay() + 6) % 7;
    date.setUTCDate(date.getUTCDate() - offset);
    date.setUTCHours(0, 0, 0, 0);
    return date.getTime();
  }

  function startOfUtcMonth(nowMs) {
    const date = new Date(nowMs);
    return Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), 1, 0, 0, 0, 0);
  }

  function startOfNextUtcMonth(nowMs) {
    const date = new Date(nowMs);
    return Date.UTC(
      date.getUTCFullYear(),
      date.getUTCMonth() + 1,
      1,
      0,
      0,
      0,
      0,
    );
  }

  function shiftMonth(year, month, delta) {
    const total = year * 12 + month + delta;
    return [Math.floor(total / 12), ((total % 12) + 12) % 12];
  }

  function anchorMonth(year, month, anchorDate) {
    const maxDay = new Date(Date.UTC(year, month + 1, 0)).getUTCDate();
    return Date.UTC(
      year,
      month,
      Math.min(anchorDate.getUTCDate(), maxDay),
      anchorDate.getUTCHours(),
      anchorDate.getUTCMinutes(),
      anchorDate.getUTCSeconds(),
      anchorDate.getUTCMilliseconds(),
    );
  }

  function anchoredMonthBounds(nowMs, anchorMs) {
    if (!Number.isFinite(anchorMs)) {
      const startMs = startOfUtcMonth(nowMs);
      return { startMs, endMs: startOfNextUtcMonth(nowMs) };
    }

    const nowDate = new Date(nowMs);
    const anchorDate = new Date(anchorMs);
    let year = nowDate.getUTCFullYear();
    let month = nowDate.getUTCMonth();
    let startMs = anchorMonth(year, month, anchorDate);

    if (startMs > nowMs) {
      const previous = shiftMonth(year, month, -1);
      year = previous[0];
      month = previous[1];
      startMs = anchorMonth(year, month, anchorDate);
    }

    const next = shiftMonth(year, month, 1);
    return {
      startMs,
      endMs: anchorMonth(next[0], next[1], anchorDate),
    };
  }

  function sumRange(rows, startMs, endMs) {
    let total = 0;
    for (let i = 0; i < rows.length; i += 1) {
      const row = rows[i];
      if (row.createdMs < startMs || row.createdMs >= endMs) continue;
      total += row.cost;
    }
    return Math.round(total * 10000) / 10000;
  }

  function nextRollingReset(rows, nowMs) {
    const startMs = nowMs - FIVE_HOURS_MS;
    let oldest = null;
    for (let i = 0; i < rows.length; i += 1) {
      const row = rows[i];
      if (row.createdMs < startMs || row.createdMs >= nowMs) continue;
      if (oldest === null || row.createdMs < oldest) oldest = row.createdMs;
    }
    return toIso((oldest === null ? nowMs : oldest) + FIVE_HOURS_MS);
  }

  function queryRows(ctx, sql) {
    const path = databasePath(ctx);
    try {
      const raw = ctx.host.sqlite.query(path, sql);
      const rows = Array.isArray(raw) ? raw : ctx.util.tryParseJson(raw);
      if (!Array.isArray(rows)) {
        ctx.host.log.warn("sqlite query returned non-array result");
        return { ok: false, rows: [] };
      }
      return { ok: true, rows };
    } catch (e) {
      ctx.host.log.warn("sqlite query failed: " + String(e));
      return { ok: false, rows: [] };
    }
  }

  function loadAuthKey(ctx) {
    const configured = trim(ctx.provider && ctx.provider.apiKey) || env(ctx, "OPENCODE_GO_API_KEY");
    if (configured) return configured;
    const inline = ctx.host.env.get("OPENCODE_AUTH_CONTENT");
    const path = inline ? null : dataPath(ctx, "auth.json");
    if (!inline && !ctx.host.fs.exists(path)) return null;

    try {
      const text = inline || ctx.host.fs.readText(path);
      const parsed = ctx.util.tryParseJson(text);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        if (inline) throw {code: "credential-malformed", message: "OPENCODE_AUTH_CONTENT must contain an auth JSON object."};
        ctx.host.log.warn("opencode auth file is not valid json");
        return null;
      }
      const entry = parsed[PROVIDER_ID];
      if (!entry || typeof entry !== "object") return null;
      const key = typeof entry.key === "string" ? entry.key.trim() : "";
      return key || null;
    } catch (e) {
      if (e && e.code) throw e;
      ctx.host.log.warn("opencode auth read failed: " + String(e));
      return null;
    }
  }

  function hasHistory(ctx) {
    const result = queryRows(ctx, HISTORY_EXISTS_SQL);
    if (!result.ok) return { ok: false, present: false };
    return { ok: true, present: result.rows.length > 0 };
  }

  function loadHistory(ctx) {
    const result = queryRows(ctx, HISTORY_ROWS_SQL);
    if (!result.ok) return result;

    const rows = [];
    for (let i = 0; i < result.rows.length; i += 1) {
      const row = result.rows[i];
      if (!row || typeof row !== "object") continue;
      const createdMs = readNumber(row.createdMs);
      const cost = readNumber(row.cost);
      if (createdMs === null || createdMs <= 0) continue;
      if (cost === null || cost < 0) continue;
      rows.push({ createdMs, cost });
    }

    return { ok: true, rows };
  }

  function buildProgressLines(ctx, rows, nowMs) {
    const sessionStartMs = nowMs - FIVE_HOURS_MS;
    const weeklyStartMs = startOfUtcWeek(nowMs);
    const weeklyEndMs = weeklyStartMs + WEEK_MS;
    let earliestMs = null;
    for (let i = 0; i < rows.length; i += 1) {
      const createdMs = rows[i].createdMs;
      if (!Number.isFinite(createdMs)) continue;
      if (earliestMs === null || createdMs < earliestMs) earliestMs = createdMs;
    }
    const monthBounds = anchoredMonthBounds(nowMs, earliestMs);
    const monthlyStartMs = monthBounds.startMs;
    const monthlyEndMs = monthBounds.endMs;

    const sessionCost = sumRange(rows, sessionStartMs, nowMs);
    const weeklyCost = sumRange(rows, weeklyStartMs, weeklyEndMs);
    const monthlyCost = sumRange(rows, monthlyStartMs, monthlyEndMs);

    return [
      ctx.line.progress({
        label: "Session",
        used: clampPercent(sessionCost, LIMITS.session),
        limit: 100,
        format: { kind: "percent" },
        resetsAt: nextRollingReset(rows, nowMs),
        periodDurationMs: FIVE_HOURS_MS,
      }),
      ctx.line.progress({
        label: "Weekly",
        used: clampPercent(weeklyCost, LIMITS.weekly),
        limit: 100,
        format: { kind: "percent" },
        resetsAt: toIso(weeklyEndMs),
        periodDurationMs: WEEK_MS,
      }),
      ctx.line.progress({
        label: "Monthly",
        used: clampPercent(monthlyCost, LIMITS.monthly),
        limit: 100,
        format: { kind: "percent" },
        resetsAt: toIso(monthlyEndMs),
        periodDurationMs: monthlyEndMs - monthlyStartMs,
      }),
    ];
  }

  function buildSoftEmptyLines(ctx) {
    return [
      ctx.line.badge({
        label: "Status",
        text: "No usage data",
        color: "#a3a3a3",
      }),
    ];
  }

  function regexNumber(text, regex) {
    const match = regex.exec(String(text || ""));
    return match ? readNumber(match[1]) : null;
  }

  function extractWindow(text, names) {
    for (let i = 0; i < names.length; i += 1) {
      const name = names[i];
      const percent = regexNumber(
        text,
        new RegExp(name + "[^}]*?(?:usagePercent|usedPercent|percentUsed|percent)\\s*[:=]\\s*([0-9]+(?:\\.[0-9]+)?)")
      );
      if (percent === null) continue;
      const reset = regexNumber(
        text,
        new RegExp(name + "[^}]*?(?:resetInSec|resetInSeconds|resetSeconds|resetSec)\\s*[:=]\\s*([0-9]+)")
      ) || 0;
      return {
        percent: Math.max(0, Math.min(100, percent)),
        resetsAt: reset > 0 ? toIso(Date.now() + reset * 1000) : null,
      };
    }
    return null;
  }

  function dateFromText(value) {
    const text = trim(value);
    if (!text) return null;
    const n = readNumber(text);
    if (n !== null && n > 0) return toIso(n > 10000000000 ? n : n * 1000);
    const ms = Date.parse(text);
    return Number.isFinite(ms) ? toIso(ms) : null;
  }

  function extractRenewal(text) {
    const match = /(?:"renewAt"|"renew_at"|renewAt|renew_at)\s*[:=]\s*"?([^",}\s]+)"?/.exec(String(text || ""));
    return match ? dateFromText(match[1]) : null;
  }

  function findBalanceValue(value) {
    if (Array.isArray(value)) {
      for (let i = 0; i < value.length; i += 1) {
        const found = findBalanceValue(value[i]);
        if (found !== null) return found;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    const keys = Object.keys(value);
    for (let i = 0; i < keys.length; i += 1) {
      const key = keys[i];
      const normalized = key.toLowerCase().replace(/[^a-z0-9]/g, "");
      if (
        normalized === "zenbalance" ||
        normalized === "zencurrentbalance" ||
        normalized === "currentbalance" ||
        normalized === "currentbalanceusd" ||
        normalized === "balanceusd" ||
        normalized === "usdbalance"
      ) {
        const parsed = readNumber(typeof value[key] === "string" ? value[key].replace(/,/g, "") : value[key]);
        if (parsed !== null) return parsed;
      }
      const nested = findBalanceValue(value[key]);
      if (nested !== null) return nested;
    }
    return null;
  }

  function parseZenBalance(ctx, text) {
    const json = ctx.util.tryParseJson(text);
    if (json) {
      const found = findBalanceValue(json);
      if (found !== null) return found;
    }
    const patterns = [
      /(?:current\s+balance|zen\s+balance|現在の残高)[^$]{0,80}\$\s*([0-9][0-9,]*(?:\.[0-9]+)?)/i,
      /(?:balance|残高)[\s\S]{0,120}?\$\s*([0-9][0-9,]*(?:\.[0-9]+)?)/i,
    ];
    for (let i = 0; i < patterns.length; i += 1) {
      const match = patterns[i].exec(String(text || ""));
      if (!match) continue;
      const parsed = readNumber(match[1].replace(/,/g, ""));
      if (parsed !== null) return parsed;
    }
    return null;
  }

  function buildWebLines(ctx, text) {
    const rolling = extractWindow(text, ["rollingUsage", "rolling_usage", "rolling"]);
    const weekly = extractWindow(text, ["weeklyUsage", "weekly_usage", "weekly"]);
    const monthly = extractWindow(text, ["monthlyUsage", "monthly_usage", "monthly"]);
    const lines = [];
    if (rolling) {
      lines.push(ctx.line.progress({
        label: "Session",
        used: rolling.percent,
        limit: 100,
        format: { kind: "percent" },
        resetsAt: rolling.resetsAt,
        periodDurationMs: FIVE_HOURS_MS,
      }));
    }
    if (weekly) {
      lines.push(ctx.line.progress({
        label: "Weekly",
        used: weekly.percent,
        limit: 100,
        format: { kind: "percent" },
        resetsAt: weekly.resetsAt,
        periodDurationMs: WEEK_MS,
      }));
    }
    if (monthly) {
      lines.push(ctx.line.progress({
        label: "Monthly",
        used: monthly.percent,
        limit: 100,
        format: { kind: "percent" },
        resetsAt: monthly.resetsAt,
      }));
    }
    const renewsAt = extractRenewal(text);
    if (renewsAt) lines.push(ctx.line.text({ label: "Renews", value: renewsAt }));
    const balance = parseZenBalance(ctx, text);
    if (balance !== null) {
      lines.push(ctx.line.text({ label: "Zen balance", value: "$" + balance.toFixed(2) }));
    }
    return { lines, balance };
  }

  function fetchWebResult(ctx, cookie) {
    const workspaceId = workspaceOverride(ctx) || fetchWorkspaceId(ctx, cookie);
    const page = fetchUsagePage(ctx, workspaceId, cookie);
    const parsed = buildWebLines(ctx, page);
    if (!parsed.lines.length) {
      throw "OpenCode Go parse error: missing usage and balance fields.";
    }
    return { plan: "Go", source: "web", lines: parsed.lines };
  }

  function fetchApiResult(ctx, key) {
    const result = ctx.util.requestJson({
      method: "GET", url: BASE_URL + "/zen/go/v1/usage",
      headers: { Authorization: "Bearer " + key, Accept: "application/json" }, timeoutMs: 15000,
    });
    if (result.resp.status === 403 && result.json && result.json.error && result.json.error.type === "EntitlementError")
      throw "No OpenCode Go subscription on this key.";
    if (ctx.util.isAuthStatus(result.resp.status)) throw "OpenCode Go key was rejected. Sign into OpenCode Go again.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "OpenCode Go usage API returned HTTP " + result.resp.status + ".";
    const usage = result.json && result.json.usage;
    const specs = [["rolling", "Session", FIVE_HOURS_MS], ["weekly", "Weekly", WEEK_MS], ["monthly", "Monthly", null]];
    const lines = [];
    for (const spec of specs) {
      const window = usage && usage[spec[0]];
      if (window == null && spec[0] !== "rolling") continue;
      const percent = window && readNumber(window.percent);
      if (!Number.isFinite(percent)) throw "OpenCode Go usage API returned an invalid quota.";
      const resetInSec = readNumber(window.resetInSec);
      const resetsAt = ctx.util.toIso(window.resetsAt) || (resetInSec !== null && resetInSec >= 0
        ? ctx.util.toIso(Date.parse(ctx.nowIso) + resetInSec * 1000) : null);
      lines.push(ctx.line.progress({ label: spec[1], used: Math.max(0, Math.min(100, percent)), limit: 100,
        format: { kind: "percent" }, resetsAt, periodDurationMs: spec[2] }));
    }
    return { plan: "Go", source: "api", lines };
  }

  function probe(ctx) {
    const source = ctx.sourceMode || "auto";
    if (source === "web") {
      const cookie = cookieHeader(ctx);
      if (!cookie) throw "OpenCode Go session cookie is missing.";
      return fetchWebResult(ctx, cookie);
    }
    const authKey = loadAuthKey(ctx);
    if (source === "api" && !authKey) throw "OpenCode Go API key missing. Set OPENCODE_GO_API_KEY.";
    if (authKey && source !== "local") {
      const result = fetchApiResult(ctx, authKey);
      try {
        const history = loadHistory(ctx);
        if (history.ok) attachCostHistory(ctx, result.lines, history.rows);
      } catch (_) { ctx.host.log.warn("OpenCode Go local cost history unavailable"); }
      return result;
    }
    const history = hasHistory(ctx);
    const cookie = cookieHeader(ctx);
    const detected = !!authKey || !!cookie || (history.ok && history.present);

    if (!detected) {
      throw "OpenCode Go not detected. Log in with OpenCode Go or use it locally first.";
    }

    if (!history.ok && !cookie) {
      return { plan: "Go", source: "local", lines: buildSoftEmptyLines(ctx) };
    }

    const rowsResult = loadHistory(ctx);
    if (rowsResult.ok && rowsResult.rows.length > 0) {
      const lines = buildProgressLines(ctx, rowsResult.rows, readNowMs());
      for (const line of lines) line.detail = "Estimated from local API-rate costs";
      attachCostHistory(ctx, lines, rowsResult.rows);
      if (cookie && source !== "local") {
        try {
          const webResult = fetchWebResult(ctx, cookie);
          for (let i = 0; i < webResult.lines.length; i += 1) {
            if (webResult.lines[i].label === "Zen balance" || webResult.lines[i].label === "Renews") {
              lines.push(webResult.lines[i]);
            }
          }
        } catch (e) {
          ctx.host.log.warn("OpenCode Go web enrichment failed: " + String(e));
        }
      }
      return { plan: "Go", source: "local-estimate", lines };
    }

    if (cookie && source !== "local") {
      try {
        return fetchWebResult(ctx, cookie);
      } catch (e) {
        ctx.host.log.warn("OpenCode Go web fallback failed: " + String(e));
      }
    }

    if (!rowsResult.ok) {
      return { plan: "Go", source: "local", lines: buildSoftEmptyLines(ctx) };
    }

    return {
      plan: "Go",
      source: "local",
      lines: buildSoftEmptyLines(ctx),
    };
  }

  function attachCostHistory(ctx, lines, rows) {
    const byDay = {};
    const now = Date.parse(ctx.nowIso);
    const since = Date.parse(ctx.nowIso.slice(0, 10)) - 29 * 86400000;
    for (const row of rows) {
      if (row.createdMs < since || row.createdMs > now) continue;
      const date = toIso(row.createdMs);
      if (!date) continue;
      const day = date.slice(0, 10);
      byDay[day] = (byDay[day] || 0) + row.cost;
    }
    const daily = Object.keys(byDay).sort().map((day) => ({ date: day, costUsd: byDay[day] }));
    if (!daily.length) return;
    lines.push(ctx.line.text({ label: "Local Cost", value: "$" + daily.reduce((n, day) => n + day.costUsd, 0).toFixed(2), subtitle: "Last 30 days; estimated API-rate cost" }));
    lines.push(ctx.line.barChart({ label: "Cost History", points: daily.map((day) => ({ label: day.date, value: day.costUsd, valueLabel: "$" + day.costUsd.toFixed(2) })),
      note: "Estimated from local OpenCode Go messages, not a subscription bill.", color: "#4d9f75" }));
    try {
      ctx.host.usageDaily.ingest({ displayName: "OpenCode Go", source: "opencode_go_local_estimated", daily });
    } catch (_) { ctx.host.log.warn("Could not persist OpenCode Go daily costs"); }
  }

  globalThis.__openusage_plugin = { id: PROVIDER_ID, probe };
})();
