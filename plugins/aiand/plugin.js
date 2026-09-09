(function () {
  var LOGS_URL = "https://api.aiand.com/logs";
  var PAGE_LIMIT = 100;
  var MAX_PAGES = 10;

  function clean(value) {
    if (typeof value !== "string") return null;
    var v = value.trim();
    if (!v) return null;
    if ((v[0] === "\"" && v[v.length - 1] === "\"") || (v[0] === "'" && v[v.length - 1] === "'")) {
      v = v.slice(1, -1).trim();
    }
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
    return clean(ctx.provider && ctx.provider.apiKey) || env(ctx, "AIAND_API_KEY");
  }

  function requestPage(ctx, key, after, afterId) {
    var url = LOGS_URL + "?range=30days&limit=" + PAGE_LIMIT;
    if (after) url += "&after=" + encodeURIComponent(after);
    if (afterId) url += "&after_id=" + encodeURIComponent(afterId);
    var result = ctx.util.requestJson({
      method: "GET",
      url: url,
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    if (result.resp.status === 401) throw "ai& rejected the API key. Create a new key at console.aiand.com.";
    if (result.resp.status === 402) throw "ai& reports the organization is out of credits.";
    if (result.resp.status === 429) throw "ai& rate limit exceeded. Usage will refresh on the next cycle.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "ai& logs API returned HTTP " + result.resp.status + ".";
    if (!result.json || typeof result.json !== "object") throw "Could not parse ai& usage.";
    return result.json;
  }

  function summarize(rows, complete) {
    var currency = null;
    var total = 0;
    for (var i = 0; i < rows.length; i++) {
      var row = rows[i] || {};
      var raw = row.cost;
      var cost = Number(raw);
      var code = typeof row.currency === "string" ? row.currency.trim().toUpperCase() : "";
      if ((typeof raw !== "number" && typeof raw !== "string") || String(raw).trim() === "" || !Number.isFinite(cost) || cost < 0 || !code) {
        complete = false;
        continue;
      }
      if (!currency) currency = code;
      if (currency !== code) throw "ai& returned mixed currencies; spend cannot be combined.";
      total += cost;
    }
    return { currency: currency, total: total, complete: complete };
  }

  function money(value, currency) {
    if (currency === "USD") return "$" + value.toFixed(2);
    return currency + " " + value.toFixed(2);
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "Missing ai& API key. Set AIAND_API_KEY or provider apiKey.";

    var rows = [];
    var after = null;
    var afterId = null;
    var complete = false;
    var cursors = {};
    for (var pageIndex = 0; pageIndex < MAX_PAGES; pageIndex++) {
      var page = requestPage(ctx, key, after, afterId);
      if (!Array.isArray(page.data)) throw "ai& logs response is missing requests.";
      rows = rows.concat(page.data);
      if (!page.has_more) {
        complete = true;
        break;
      }
      after = typeof page.next_after === "string" && page.next_after.trim() ? page.next_after.trim() : null;
      afterId = typeof page.next_after_id === "string" && page.next_after_id.trim() ? page.next_after_id.trim() : null;
      if (!after || !afterId) break;
      var cursorKey = JSON.stringify([after, afterId]);
      if (cursors[cursorKey]) break;
      cursors[cursorKey] = true;
    }

    var summary = summarize(rows, complete);
    var period = summary.complete ? "Last 30 days" : "Last 30 days (partial)";
    var metrics = [];
    if (summary.currency) {
      metrics.push(ctx.line.text({ label: "Cost", value: money(summary.total, summary.currency), subtitle: period }));
    } else {
      metrics.push(ctx.line.badge({ label: "Cost", text: "No priced requests", color: "#a3a3a3", subtitle: period }));
    }
    return { displayName: "ai&", source: "api", lines: metrics };
  }

  globalThis.__openusage_plugin = { id: "aiand", probe: probe };
})();
