(function () {
  var BASE_URL = "https://management-api.x.ai";
  var HISTORY_DAYS = 30;

  function clean(value) {
    if (typeof value !== "string") return null;
    var v = value.trim();
    if (!v) return null;
    if ((v[0] === "\"" && v[v.length - 1] === "\"") || (v[0] === "'" && v[v.length - 1] === "'")) v = v.slice(1, -1).trim();
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
    return clean(ctx.provider && ctx.provider.apiKey) || env(ctx, "XAI_MANAGEMENT_API_KEY");
  }

  function teamId(ctx) {
    var id = clean(ctx.provider && ctx.provider.workspaceId) || env(ctx, "XAI_TEAM_ID");
    if (!id) return null;
    if (id === "." || id === ".." || id.indexOf("/") >= 0 || id.indexOf("\\") >= 0) {
      throw "The xAI team ID must be a single identifier without path separators.";
    }
    return id;
  }

  function requestJson(ctx, req) {
    var result = ctx.util.requestJson(req);
    if (ctx.util.isAuthStatus(result.resp.status)) {
      throw "xAI rejected the Management API key. Create one in the xAI Console under Settings > Management Keys.";
    }
    if (result.resp.status === 404) throw "xAI returned 404 for this team. Check the team ID and Management key.";
    if (result.resp.status === 429) throw "xAI Management API rate limit exceeded.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "xAI Management API returned HTTP " + result.resp.status + ".";
    if (!result.json || typeof result.json !== "object") throw "Could not parse xAI billing data.";
    return result.json;
  }

  function balanceUsd(raw) {
    var text = raw == null ? "" : String(raw).trim();
    if (!/^-?\d+(?:\.\d+)?$/.test(text) || !Number.isFinite(Number(text))) throw "xAI balance total.val is not a cent amount.";
    return -Number(text) / 100;
  }

  function pad(n) {
    return n < 10 ? "0" + n : String(n);
  }

  function formatRequestTimestamp(date) {
    return date.getUTCFullYear() + "-" + pad(date.getUTCMonth() + 1) + "-" + pad(date.getUTCDate()) +
      " " + pad(date.getUTCHours()) + ":" + pad(date.getUTCMinutes()) + ":" + pad(date.getUTCSeconds());
  }

  function usageBody(now) {
    var start = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()));
    start = new Date(start.getTime() - (HISTORY_DAYS - 1) * 24 * 60 * 60 * 1000);
    return JSON.stringify({
      analyticsRequest: {
        timeRange: {
          startTime: formatRequestTimestamp(start),
          endTime: formatRequestTimestamp(now),
          timezone: "Etc/GMT",
        },
        timeUnit: "TIME_UNIT_DAY",
        values: [{ name: "usd", aggregation: "AGGREGATION_SUM" }],
        groupBy: [],
        filters: [],
      },
    });
  }

  function dayKey(ctx, timestamp) {
    var iso = ctx.util.toIso(timestamp);
    if (typeof timestamp === "string" && /^\d{4}-\d{2}-\d{2}$/.test(timestamp) && iso && iso.slice(0, 10) !== timestamp) return null;
    return iso ? iso.slice(0, 10) : null;
  }

  function shortDay(day) {
    return Number(day.slice(5, 7)) + "/" + Number(day.slice(8, 10));
  }

  function fetchDaily(ctx, key, id) {
    try {
      var json = requestJson(ctx, {
        method: "POST",
        url: BASE_URL + "/v1/billing/teams/" + encodeURIComponent(id) + "/usage",
        headers: {
          Authorization: "Bearer " + key,
          Accept: "application/json",
          "Content-Type": "application/json",
        },
        bodyText: usageBody(new Date(ctx.nowIso)),
        timeoutMs: 3000,
      });
      var totals = {};
      if (!Array.isArray(json.timeSeries)) throw "Missing xAI usage time series.";
      var series = json.timeSeries;
      for (var i = 0; i < series.length; i++) {
        var points = Array.isArray(series[i].dataPoints) ? series[i].dataPoints : [];
        for (var j = 0; j < points.length; j++) {
          var day = dayKey(ctx, points[j].timestamp);
          var raw = Array.isArray(points[j].values) ? points[j].values[0] : null;
          if (!day || (typeof raw !== "number" && typeof raw !== "string") || String(raw).trim() === "" || !Number.isFinite(Number(raw)) || Number(raw) < 0) throw "Invalid xAI usage history point.";
          if (day <= ctx.nowIso.slice(0, 10)) totals[day] = (totals[day] || 0) + Number(raw);
        }
      }
      return Object.keys(totals).sort().map(function (day) {
        return { day: day, cost: totals[day] };
      });
    } catch (error) {
      ctx.host.log.warn("xAI usage history request failed: " + String(error));
      return [];
    }
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "Missing xAI Management API key. Set XAI_MANAGEMENT_API_KEY or provider apiKey.";
    var id = teamId(ctx);
    if (!id) throw "Missing xAI team ID. Set XAI_TEAM_ID or provider workspaceId.";

    var balanceJson = requestJson(ctx, {
      method: "GET",
      url: BASE_URL + "/v1/billing/teams/" + encodeURIComponent(id) + "/prepaid/balance",
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    var balance = balanceUsd(balanceJson.total && balanceJson.total.val);
    var metrics = [ctx.line.text({ label: "Balance", value: balance < 0 ? "Deficit: $" + (-balance).toFixed(2) : "$" + balance.toFixed(2) })];

    var daily = fetchDaily(ctx, key, id);
    if (daily.length) {
      try {
        ctx.host.usageDaily.ingest({ provider: "xai", source: "api", daily: daily.map(function (row) {
          return { date: row.day, costUsd: row.cost };
        }) });
      } catch (error) {
        ctx.host.log.warn("Could not persist xAI usage history: " + String(error));
      }
      var total = daily.reduce(function (sum, row) { return sum + row.cost; }, 0);
      metrics.push(ctx.line.text({ label: "Cost", value: "$" + total.toFixed(2), subtitle: "Last 30 days" }));
      metrics.push(ctx.line.barChart({
        label: "Cost History",
        points: daily.map(function (row) { return { label: shortDay(row.day), value: row.cost, valueLabel: "$" + row.cost.toFixed(2) }; }),
        note: "Recent xAI Management API usage.",
        color: "#111111",
      }));
    }

    return { displayName: "xAI", source: "api", plan: "Management API", lines: metrics };
  }

  globalThis.__openusage_plugin = { id: "xai", probe: probe };
})();
