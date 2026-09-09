(function () {
  var DEFAULT_BASE = "https://openrouter.ai/api/v1";
  var ACTIVITY_URL = DEFAULT_BASE + "/activity";
  var ACTIVITY_DAYS = 30;
  var MAX_ACTIVITY_ROWS = 20000;

  function clean(value) {
    if (typeof value !== "string") return null;
    var v = value.trim();
    if (!v) return null;
    if (
      v.length >= 2 &&
      ((v.charAt(0) === '"' && v.charAt(v.length - 1) === '"') ||
        (v.charAt(0) === "'" && v.charAt(v.length - 1) === "'"))
    ) {
      v = v.slice(1, -1).trim();
    }
    return v || null;
  }

  function env(ctx, name) {
    try {
      return clean(ctx.host.env.get(name));
    } catch (_) {
      return null;
    }
  }

  function setting(ctx, names) {
    var settings = ctx.provider && ctx.provider.settings ? ctx.provider.settings : {};
    for (var i = 0; i < names.length; i++) {
      var value = clean(settings[names[i]]);
      if (value) return value;
    }
    return null;
  }

  function readApiKey(ctx) {
    return clean(ctx.provider && ctx.provider.apiKey) || env(ctx, "OPENROUTER_API_KEY");
  }

  function readManagementKey(ctx) {
    return setting(ctx, ["managementApiKey", "managementKey", "activityApiKey"]) ||
      env(ctx, "OPENROUTER_MANAGEMENT_API_KEY");
  }

  function readApiBase(ctx) {
    var value = env(ctx, "OPENROUTER_API_BASE") || setting(ctx, ["apiBase", "baseUrl", "apiUrl"]);
    var base = (value || DEFAULT_BASE).replace(/\/+$/, "");
    if (/\/auth$/i.test(base)) base = base.slice(0, -5);
    return ctx.host.http.validateBaseUrl(base || DEFAULT_BASE, true);
  }

  function requestJson(ctx, request) {
    var resp;
    try {
      resp = ctx.host.http.request(request);
    } catch (error) {
      ctx.host.log.error("HTTP request failed: " + String(error));
      throw "OpenRouter request failed. Check your connection.";
    }

    var json = null;
    try {
      json = resp.bodyText ? JSON.parse(resp.bodyText) : null;
    } catch (_) {
      throw "OpenRouter response was not valid JSON.";
    }

    return { resp: resp, json: json };
  }

  function fetchOpenRouterJson(ctx, url, key, timeoutMs) {
    var result = requestJson(ctx, {
      method: "GET",
      url: url,
      headers: {
        Authorization: "Bearer " + key,
        Accept: "application/json",
      },
      timeoutMs: timeoutMs || 15000,
    });
    if (result.resp.status === 401 || result.resp.status === 403) {
      throw "OpenRouter API key is invalid.";
    }
    if (result.resp.status < 200 || result.resp.status >= 300) {
      throw "OpenRouter request failed (HTTP " + result.resp.status + ").";
    }
    return result.json;
  }

  function readNumber(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.trim().replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function clamp(value, min, max) {
    return Math.max(min, Math.min(max, value));
  }

  function round1(value) {
    return Math.round(value * 10) / 10;
  }

  function round2(value) {
    return Math.round(value * 100) / 100;
  }

  function money(value) {
    return "$" + round2(value || 0).toFixed(2);
  }

  function progressLine(label, used, limit, format) {
    return {
      type: "progress",
      label: label,
      used: used,
      limit: limit,
      format: format || { kind: "percent" },
    };
  }

  function appendCreditsMetrics(metrics, data) {
    var totalCredits = readNumber(data.total_credits);
    var totalUsage = readNumber(data.total_usage);

    if (totalCredits === null || totalUsage === null) {
      throw "OpenRouter credits response did not include usage totals.";
    }

    var remaining = Math.max(0, totalCredits - totalUsage);
    var usedPercent = totalCredits > 0 ? clamp((totalUsage / totalCredits) * 100, 0, 100) : 0;
    metrics.push(progressLine("Credits", round2(totalUsage), round2(totalCredits), { kind: "dollars" }));
    metrics.push({ type: "text", label: "Balance", value: money(remaining) });
    metrics.push({
      type: "text",
      label: "Used",
      value: money(totalUsage) + " (" + round1(usedPercent) + "%)",
    });
  }

  function quotaFallbackUsage(data) {
    var reset = typeof data.limit_reset === "string" ? data.limit_reset.toLowerCase() : "";
    if (reset === "daily") return readNumber(data.usage_daily);
    if (reset === "weekly") return readNumber(data.usage_weekly);
    if (reset === "monthly") return readNumber(data.usage_monthly);
    return readNumber(data.usage);
  }

  function appendKeyMetrics(metrics, data) {
    var limit = readNumber(data.limit);
    if (limit !== null && limit > 0) {
      var remaining = readNumber(data.limit_remaining);
      if (remaining === null) remaining = readNumber(data.limitRemaining);
      var used = null;
      if (remaining !== null) {
        used = limit - clamp(remaining, 0, limit);
      } else {
        used = quotaFallbackUsage(data);
      }
      if (used !== null && used >= 0 && Number.isFinite(used)) {
        metrics.push(progressLine("Key Cap", round2(used), round2(limit), { kind: "dollars" }));
      }
    }

    appendSpendText(metrics, "Daily Spend", data.usage_daily, "today");
    appendSpendText(metrics, "Weekly Spend", data.usage_weekly, "this week");
    appendSpendText(metrics, "Monthly Spend", data.usage_monthly, "this month");

    var rateLimit = data.rate_limit || data.rateLimit;
    if (rateLimit && rateLimit.requests !== undefined) {
      var interval = typeof rateLimit.interval === "string" ? " / " + rateLimit.interval : "";
      metrics.push({
        type: "text",
        label: "Rate Limit",
        value: String(rateLimit.requests) + " requests" + interval,
      });
    }
  }

  function appendSpendText(metrics, label, value, subtitle) {
    var n = readNumber(value);
    if (n === null || n < 0) return;
    metrics.push({ type: "text", label: label, value: money(n), subtitle: subtitle });
  }

  function parseDay(value) {
    if (typeof value === "string") {
      var trimmed = value.trim();
      var match = trimmed.match(/^(\d{4}-\d{2}-\d{2})/);
      if (match) {
        var calendarMs = Date.parse(match[1] + "T00:00:00Z");
        return Number.isFinite(calendarMs) && new Date(calendarMs).toISOString().slice(0, 10) === match[1] ? match[1] : null;
      }
      var ms = Date.parse(trimmed);
      if (Number.isFinite(ms)) return new Date(ms).toISOString().slice(0, 10);
    }
    if (typeof value === "number" && Number.isFinite(value)) {
      var date = new Date(value > 1000000000000 ? value : value * 1000);
      return Number.isFinite(date.getTime()) ? date.toISOString().slice(0, 10) : null;
    }
    return null;
  }

  function dayToMs(day) {
    return Date.parse(day + "T00:00:00Z");
  }

  function shortDay(day) {
    return Number(day.slice(5, 7)) + "/" + Number(day.slice(8, 10));
  }

  function latestCompletedUtcDay(ctx) {
    var nowMs = Date.parse(ctx.nowIso);
    if (!Number.isFinite(nowMs)) nowMs = Date.now();
    var now = new Date(nowMs);
    var today = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
    return new Date(today - 24 * 60 * 60 * 1000).toISOString().slice(0, 10);
  }

  function activityRowCost(row, index) {
    var metered = readNumber(row.usage);
    if (metered === null || metered < 0) throw "OpenRouter Activity usage is invalid at row " + index + ".";
    var estimated = readNumber(row.byok_usage_inference);
    if (estimated === null && row.byok_usage_inference != null) throw "OpenRouter Activity BYOK usage is invalid at row " + index + ".";
    if (estimated === null) estimated = 0;
    if (estimated < 0) throw "OpenRouter Activity BYOK usage is invalid at row " + index + ".";
    var total = metered + estimated;
    if (!Number.isFinite(total)) throw "OpenRouter Activity cost total is invalid.";
    return { metered: metered, estimated: estimated, total: total };
  }

  function tokenCount(value) {
    if (value == null) return 0;
    var n = readNumber(value);
    if (!Number.isSafeInteger(n) || n < 0) throw "OpenRouter Activity token count is invalid.";
    return n;
  }

  function rowId(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    return clean(value) || "";
  }

  function summarizeActivityPayloads(payloads, latestCompleted) {
    var latestMs = dayToMs(latestCompleted);
    var cutoffMs = latestMs - (ACTIVITY_DAYS - 1) * 24 * 60 * 60 * 1000;
    var seen = {};
    var byDay = {};
    var total = 0;
    var estimated = 0;
    var rowsSeen = 0;
    var aggregateTokens = 0;
    var aggregateRequests = 0;

    for (var p = 0; p < payloads.length; p++) {
      var rows = payloads[p] && Array.isArray(payloads[p].data) ? payloads[p].data : null;
      if (!rows) throw "OpenRouter Activity response missing data array.";
      rowsSeen += rows.length;
      if (rowsSeen > MAX_ACTIVITY_ROWS) throw "OpenRouter Activity returned too many rows.";
      for (var i = 0; i < rows.length; i++) {
        var row = rows[i];
        if (!row || typeof row !== "object") throw "OpenRouter Activity row is invalid.";
        var day = parseDay(row.date);
        if (!day) throw "OpenRouter Activity row is missing a usable date.";
        var dayMs = dayToMs(day);
        if (dayMs < cutoffMs || dayMs > latestMs) continue;
        var model = clean(row.model_permaslug) || clean(row.model) || "";
        if (model.length > 512) throw "OpenRouter Activity model name is too long.";
        var cost = activityRowCost(row, i);
        var input = tokenCount(row.prompt_tokens);
        var output = tokenCount(row.completion_tokens);
        var combined = tokenCount(input + output);
        var reasoning = tokenCount(row.reasoning_tokens);
        if (reasoning > output) throw "OpenRouter Activity reasoning tokens exceed completion tokens.";
        var identity = JSON.stringify([
          day,
          model,
          rowId(row.endpoint_id),
          clean(row.provider_name) || "",
          clean(row.workspace_id) || "",
        ]);
        var signature = JSON.stringify([input, output, reasoning, tokenCount(row.requests), cost.metered, cost.estimated]);
        if (seen[identity]) {
          if (seen[identity] !== signature) throw "OpenRouter Activity contains conflicting duplicate rows.";
          continue;
        }
        seen[identity] = signature;
        aggregateTokens = tokenCount(aggregateTokens + combined);
        aggregateRequests = tokenCount(aggregateRequests + tokenCount(row.requests));
        total += cost.total;
        estimated += cost.estimated;
        if (!Number.isFinite(total) || !Number.isFinite(estimated)) throw "OpenRouter Activity cost total is invalid.";
        var daily = byDay[day] || { date: day, costUsd: 0, inputTokens: 0, outputTokens: 0, reasoningOutputTokens: 0, totalTokens: 0 };
        daily.costUsd += cost.total;
        daily.inputTokens = tokenCount(daily.inputTokens + input);
        // Reasoning is a subset of completion tokens; the history store sums these fields.
        daily.outputTokens = tokenCount(daily.outputTokens + (output - reasoning));
        daily.reasoningOutputTokens = tokenCount(daily.reasoningOutputTokens + reasoning);
        daily.totalTokens = tokenCount(daily.totalTokens + combined);
        byDay[day] = daily;
      }
    }

    return { total: total, estimated: estimated, byDay: byDay };
  }

  function fetchActivityPayload(ctx, managementKey, date) {
    var url = ACTIVITY_URL;
    if (date) url += "?date=" + encodeURIComponent(date);
    var result = requestJson(ctx, {
      method: "GET",
      url: url,
      headers: {
        Authorization: "Bearer " + managementKey,
        Accept: "application/json",
      },
      timeoutMs: 3000,
    });
    if (result.resp.status === 401 || result.resp.status === 403) {
      throw "OpenRouter management API key is invalid.";
    }
    if (result.resp.status < 200 || result.resp.status >= 300) {
      throw "OpenRouter Activity request failed (HTTP " + result.resp.status + ").";
    }
    return result.json;
  }

  function appendActivityMetrics(ctx, metrics, managementKey) {
    var latestCompleted = latestCompletedUtcDay(ctx);
    var summary = summarizeActivityPayloads([
      fetchActivityPayload(ctx, managementKey, null),
      fetchActivityPayload(ctx, managementKey, latestCompleted),
    ], latestCompleted);

    metrics.push({
      type: "text",
      label: "Cost",
      value: money(summary.total),
      subtitle: "Last 30 completed days (UTC)" + (summary.estimated > 0 ? "; includes " + money(summary.estimated) + " estimated BYOK inference" : ""),
    });

    var days = Object.keys(summary.byDay).sort();
    var chartPoints = [];
    var dailyRows = [];
    for (var i = 0; i < days.length; i++) {
      var amount = summary.byDay[days[i]].costUsd;
      chartPoints.push({
        label: shortDay(days[i]),
        value: amount,
        valueLabel: money(amount),
      });
      dailyRows.push(summary.byDay[days[i]]);
    }
    if (chartPoints.length) {
      metrics.push({
        type: "barChart",
        label: "Usage Trend",
        points: chartPoints,
        note: "OpenRouter Activity spend from completed UTC days." + (summary.estimated > 0 ? " Includes estimated BYOK inference." : ""),
        color: "#7c3aed",
      });
    }

    try {
      if (ctx.host.usageDaily && typeof ctx.host.usageDaily.ingest === "function" && dailyRows.length) {
        ctx.host.usageDaily.ingest({
          displayName: "OpenRouter",
          source: summary.estimated > 0 ? "openrouter_activity_estimated" : "openrouter_activity",
          daily: dailyRows,
        });
      }
    } catch (_) {}
  }

  function probe(ctx) {
    var apiKey = readApiKey(ctx);
    if (!apiKey) throw "No OPENROUTER_API_KEY found.";
    var apiBase = readApiBase(ctx);
    var metrics = [];

    var credits = fetchOpenRouterJson(ctx, apiBase + "/credits", apiKey, 15000);
    appendCreditsMetrics(metrics, credits && credits.data ? credits.data : {});

    try {
      var key = fetchOpenRouterJson(ctx, apiBase + "/key", apiKey, 1000);
      if (key && key.data) appendKeyMetrics(metrics, key.data);
    } catch (error) {
      ctx.host.log.warn("OpenRouter key details request degraded: " + String(error));
    }

    var managementKey = readManagementKey(ctx);
    if (managementKey) {
      try {
        appendActivityMetrics(ctx, metrics, managementKey);
      } catch (error) {
        ctx.host.log.warn("OpenRouter Activity degraded: " + String(error));
      }
    }

    return {
      displayName: "OpenRouter",
      source: "api",
      metrics: metrics,
    };
  }

  globalThis.__usagestat_plugin = { probe: probe };
})();
