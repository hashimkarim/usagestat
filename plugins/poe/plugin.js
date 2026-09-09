(function () {
  var BALANCE_URL = "https://api.poe.com/usage/current_balance";
  var HISTORY_URL = "https://api.poe.com/usage/points_history";

  function env(ctx, name) {
    try {
      var value = ctx.host.env.get(name);
      return typeof value === "string" && value.trim() ? value.trim() : null;
    } catch (_) {
      return null;
    }
  }

  function apiKey(ctx) {
    var configured = ctx.provider && typeof ctx.provider.apiKey === "string" ? ctx.provider.apiKey.trim() : "";
    return configured || env(ctx, "POE_API_KEY");
  }

  function numberValue(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function fmtNumber(value) {
    var n = Number(value) || 0;
    if (n >= 1000000) return (n / 1000000).toFixed(1).replace(/\.0$/, "") + "M";
    if (n >= 1000) return Math.round(n).toLocaleString("en-US");
    return String(Math.round(n * 10) / 10);
  }

  function requestJson(ctx, url, key) {
    var resp = ctx.util.request({
      method: "GET",
      url: url,
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    if (ctx.util.isAuthStatus(resp.status)) throw "Invalid or expired Poe API token.";
    if (resp.status < 200 || resp.status >= 300) {
      throw "Poe API request failed (HTTP " + String(resp.status) + ").";
    }
    var json = ctx.util.tryParseJson(resp.bodyText);
    if (!json) throw "Poe response was not valid JSON.";
    return json;
  }

  function parseDate(value) {
    if (typeof value !== "number" && (typeof value !== "string" || !value.trim())) return null;
    var numeric = Number(value);
    var date = Number.isFinite(numeric)
      ? new Date(Math.abs(numeric) > 100000000000000 ? numeric / 1000 : Math.abs(numeric) > 1000000000000 ? numeric : numeric * 1000)
      : new Date(value);
    return Number.isFinite(date.getTime()) && date.getUTCFullYear() >= 1 && date.getUTCFullYear() <= 9999 ? date : null;
  }

  function dayKey(date) {
    return date.toISOString().slice(0, 10);
  }

  function shortDay(day) {
    return Number(day.slice(5, 7)) + "/" + Number(day.slice(8, 10));
  }

  function rowsFromHistoryPage(page) {
    if (Array.isArray(page && page.data)) return page.data;
    if (Array.isArray(page && page.items)) return page.items;
    if (Array.isArray(page && page.results)) return page.results;
    return [];
  }

  function parseHistoryEntry(row) {
    if (!row || typeof row !== "object") return null;
    var createdAt = parseDate(row.creation_time ?? row.timestamp ?? row.created_at);
    if (!createdAt) return null;
    var points = numberValue(row.cost_points ?? row.points ?? row.point_cost) || 0;
    return {
      createdAt: createdAt,
      model: String(row.bot_name || row.model || "unknown").trim() || "unknown",
      usageType: String(row.usage_type || row.type || "unknown").trim() || "unknown",
      points: Math.max(0, points),
      costUSD: numberValue(row.cost_usd ?? row.usd),
      id: String(row.query_id || row.message_id || row.id || createdAt.getTime()),
    };
  }

  function fetchHistory(ctx, key, nowMs) {
    var entries = [];
    var cursor = null;
    var cutoff = nowMs - 30 * 24 * 3600 * 1000;
    for (var page = 0; page < 5; page++) {
      var url = HISTORY_URL + "?limit=100";
      if (cursor) url += "&starting_after=" + encodeURIComponent(cursor);
      var json = requestJson(ctx, url, key);
      var rows = rowsFromHistoryPage(json);
      for (var i = 0; i < rows.length; i++) {
        var entry = parseHistoryEntry(rows[i]);
        if (entry) entries.push(entry);
      }
      cursor = typeof json.next_cursor === "string" && json.next_cursor.trim() ? json.next_cursor.trim() : null;
      if (!cursor && json.has_more && rows.length) {
        var last = rows[rows.length - 1];
        cursor = String(last.query_id || last.message_id || last.id || "").trim() || null;
      }
      var lastEntry = entries[entries.length - 1];
      if (!cursor || (lastEntry && lastEntry.createdAt.getTime() < cutoff)) break;
    }
    return entries.filter(function (entry) { return entry.createdAt.getTime() >= cutoff && entry.createdAt.getTime() <= nowMs; });
  }

  function summarizeHistory(entries) {
    var byDay = {};
    var byModel = {};
    var totalPoints = 0;
    var totalCost = 0;
    var hasCost = false;
    for (var i = 0; i < entries.length; i++) {
      var entry = entries[i];
      var day = dayKey(entry.createdAt);
      if (!byDay[day]) byDay[day] = { points: 0, requests: 0, costUSD: 0, hasCost: false };
      byDay[day].points += entry.points;
      byDay[day].requests += 1;
      byModel[entry.model] = (byModel[entry.model] || 0) + entry.points;
      totalPoints += entry.points;
      if (entry.costUSD != null) {
        byDay[day].costUSD += Math.max(0, entry.costUSD);
        byDay[day].hasCost = true;
        totalCost += Math.max(0, entry.costUSD);
        hasCost = true;
      }
    }
    var days = Object.keys(byDay).sort();
    var topModel = Object.keys(byModel).sort(function (a, b) {
      return byModel[b] - byModel[a] || a.localeCompare(b);
    })[0];
    return { byDay: byDay, days: days, topModel: topModel, totalPoints: totalPoints, totalCost: hasCost ? totalCost : null, requests: entries.length };
  }

  function appendSummary(lines, ctx, label, summary) {
    var value = fmtNumber(summary.totalPoints) + " points";
    if (summary.totalCost != null) value += " · $" + summary.totalCost.toFixed(2);
    lines.push(ctx.line.text({ label: label, value: value + " · " + summary.requests + " requests" }));
  }

  function appendHistoryLines(lines, ctx, entries, nowMs) {
    if (!entries.length) return;
    var summary = summarizeHistory(entries);
    var today = dayKey(new Date(nowMs));
    appendSummary(lines, ctx, "Today", summarizeHistory(entries.filter(function (entry) { return dayKey(entry.createdAt) === today; })));
    appendSummary(lines, ctx, "Last 7 Days", summarizeHistory(entries.filter(function (entry) { return entry.createdAt.getTime() >= nowMs - 7 * 86400000; })));
    appendSummary(lines, ctx, "Last 30 Days", summary);
    if (summary.topModel) lines.push(ctx.line.text({ label: "Top Model", value: summary.topModel }));

    var points = summary.days.map(function (day) {
      var row = summary.byDay[day];
      return { label: shortDay(day), value: row.points, valueLabel: fmtNumber(row.points) + " points" };
    });
    if (points.length) {
      lines.push(ctx.line.barChart({
        label: "Point History",
        points: points,
        note: "Recent Poe API point usage.",
        color: "#5D5CDE",
      }));
    }
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "Missing Poe API token. Set POE_API_KEY or provider apiKey.";

    var balanceJson = requestJson(ctx, BALANCE_URL, key);
    var balance = numberValue(balanceJson.current_point_balance);
    var lines = [];
    if (balance != null) {
      lines.push(ctx.line.text({ label: "Balance", value: fmtNumber(balance) + " points" }));
    } else {
      lines.push(ctx.line.badge({ label: "Status", text: "Balance unavailable", color: "yellow" }));
    }

    try {
      var nowMs = Date.parse(ctx.nowIso);
      if (!Number.isFinite(nowMs)) nowMs = Date.now();
      appendHistoryLines(lines, ctx, fetchHistory(ctx, key, nowMs), nowMs);
    } catch (e) {
      ctx.host.log.warn("poe points_history failed: " + String(e));
    }

    return { displayName: "Poe", source: "api", lines: lines };
  }

  globalThis.__openusage_plugin = { id: "poe", probe: probe };
})();
