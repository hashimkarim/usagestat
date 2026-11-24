(function () {
  var CREDIT_GRANTS_URL = "https://api.openai.com/v1/dashboard/billing/credit_grants";
  var ORG_COSTS_URL = "https://api.openai.com/v1/organization/costs";
  var ORG_COMPLETIONS_URL = "https://api.openai.com/v1/organization/usage/completions";
  var HISTORY_DAYS = 30;
  var MAX_PAGES = 100;

  function trim(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null;
  }

  function clean(value) {
    var v = trim(value);
    if (!v) return null;
    if ((v[0] === '"' && v[v.length - 1] === '"') || (v[0] === "'" && v[v.length - 1] === "'")) {
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

  function loadCredential(ctx) {
    var configured = clean(ctx.provider && ctx.provider.apiKey);
    if (configured) {
      return {
        apiKey: configured,
        usesAdminKey: true,
        projectId: loadProjectId(ctx),
      };
    }

    var names = [
      "OPENAI_ADMIN_KEY",
      "OPENAI_ADMIN_API_KEY",
      "OPENAI_API_KEY",
      "OPENAI_PLATFORM_API_KEY",
    ];
    for (var i = 0; i < names.length; i++) {
      var value = env(ctx, names[i]);
      if (value) {
        return {
          apiKey: value,
          usesAdminKey: names[i] === "OPENAI_ADMIN_KEY" || names[i] === "OPENAI_ADMIN_API_KEY",
          projectId: loadProjectId(ctx),
        };
      }
    }
    return null;
  }

  function loadProjectId(ctx) {
    return setting(ctx, ["projectId", "projectID", "project", "openaiProjectId"]) ||
      clean(ctx.provider && ctx.provider.workspaceId) ||
      env(ctx, "OPENAI_PROJECT_ID");
  }

  // Saved setting first, then the SDK environment names; blank values are ignored.
  function loadOrganizationId(ctx) {
    return setting(ctx, ["OPENAI_ORGANIZATION_ID", "organizationId", "organizationID"]) ||
      env(ctx, "OPENAI_ORGANIZATION_ID") ||
      env(ctx, "OPENAI_ORG_ID");
  }

  function numberValue(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.trim().replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function intValue(value) {
    var n = numberValue(value);
    return n === null ? 0 : Math.max(0, Math.floor(n));
  }

  function formatUsd(value) {
    var n = Number(value) || 0;
    return "$" + n.toFixed(2);
  }

  function fmtCount(n, suffix) {
    var value = Number(n) || 0;
    var abs = Math.abs(value);
    if (abs >= 1000000000) return (value / 1000000000).toFixed(1).replace(/\.0$/, "") + "B " + suffix;
    if (abs >= 1000000) return (value / 1000000).toFixed(1).replace(/\.0$/, "") + "M " + suffix;
    if (abs >= 1000) return Math.round(value).toLocaleString("en-US") + " " + suffix;
    return String(Math.round(value)) + " " + suffix;
  }

  function dayKeyFromUtcMs(ms) {
    return new Date(ms).toISOString().slice(0, 10);
  }

  function dayKeyFromSec(sec) {
    return dayKeyFromUtcMs(sec * 1000);
  }

  function shortDayLabel(day) {
    return Number(day.slice(5, 7)) + "/" + Number(day.slice(8, 10));
  }

  function adminRanges(nowMs) {
    var date = new Date(nowMs);
    var todayStartMs = Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate());
    var startMs = todayStartMs - (HISTORY_DAYS - 1) * 24 * 60 * 60 * 1000;
    var endMs = todayStartMs + 24 * 60 * 60 * 1000;
    return [{
      startTime: Math.floor(startMs / 1000),
      endTime: Math.floor(endMs / 1000),
      limit: HISTORY_DAYS,
    }];
  }

  function makeUrl(baseUrl, params) {
    var query = [];
    var keys = Object.keys(params);
    for (var i = 0; i < keys.length; i++) {
      var key = keys[i];
      var value = params[key];
      if (value === null || value === undefined || value === "") continue;
      query.push(encodeURIComponent(key) + "=" + encodeURIComponent(String(value)));
    }
    return baseUrl + (query.length ? "?" + query.join("&") : "");
  }

  function makeHttpError(message, status, endpoint) {
    return {
      message: message,
      status: status,
      endpoint: endpoint,
      authRejected: status === 401 || status === 403,
    };
  }

  function requestJson(ctx, url, apiKey, endpoint, extraHeaders) {
    var headers = { Authorization: "Bearer " + apiKey, Accept: "application/json" };
    if (extraHeaders) {
      for (var name in extraHeaders) headers[name] = extraHeaders[name];
    }
    var resp;
    try {
      resp = ctx.util.request({
        method: "GET",
        url: url,
        headers: headers,
        timeoutMs: 20000,
      });
    } catch (e) {
      throw makeHttpError("OpenAI API " + endpoint + " request failed: " + String(e), 0, endpoint);
    }

    if (ctx.util.isAuthStatus(resp.status)) {
      throw makeHttpError("OpenAI API key invalid or missing required permissions.", resp.status, endpoint);
    }
    if (resp.status < 200 || resp.status >= 300) {
      throw makeHttpError("OpenAI API " + endpoint + " error (HTTP " + resp.status + ").", resp.status, endpoint);
    }

    var json = ctx.util.tryParseJson(resp.bodyText);
    if (!json) throw makeHttpError("Could not parse OpenAI API " + endpoint + " response.", resp.status, endpoint);
    return json;
  }

  function fetchPagedBuckets(ctx, endpoint) {
    var buckets = [];
    var ranges = adminRanges(Date.now());
    for (var r = 0; r < ranges.length; r++) {
      var range = ranges[r];
      var nextPage = null;
      var seen = {};
      var pageCount = 0;
      do {
        pageCount += 1;
        if (pageCount > MAX_PAGES) {
          throw makeHttpError("OpenAI API " + endpoint.name + " pagination exceeded " + MAX_PAGES + " pages.", 200, endpoint.name);
        }

        var params = {
          start_time: range.startTime,
          end_time: range.endTime,
          bucket_width: "1d",
          limit: range.limit,
          group_by: endpoint.groupBy,
        };
        if (endpoint.projectId) params.project_ids = endpoint.projectId;
        if (nextPage) params.page = nextPage;

        var json = requestJson(ctx, makeUrl(endpoint.url, params), endpoint.apiKey, endpoint.name);
        if (Array.isArray(json.data)) {
          for (var i = 0; i < json.data.length; i++) buckets.push(json.data[i]);
        }

        if (!json.has_more) {
          nextPage = null;
          continue;
        }
        nextPage = clean(json.next_page);
        if (!nextPage) {
          throw makeHttpError("OpenAI API " + endpoint.name + " pagination cursor missing.", 200, endpoint.name);
        }
        if (seen[nextPage]) {
          throw makeHttpError("OpenAI API " + endpoint.name + " pagination cursor repeated.", 200, endpoint.name);
        }
        seen[nextPage] = true;
      } while (nextPage);
    }
    return buckets;
  }

  function displayName(raw, fallback) {
    return clean(raw) || fallback;
  }

  function dailyBucket(map, startTime, endTime) {
    var key = dayKeyFromSec(startTime);
    if (!map[key]) {
      map[key] = {
        date: key,
        startTime: startTime,
        endTime: endTime,
        costUSD: 0,
        requests: 0,
        inputTokens: 0,
        cachedInputTokens: 0,
        outputTokens: 0,
        totalTokens: 0,
        lineItems: {},
        models: {},
      };
    }
    if (endTime && (!map[key].endTime || endTime > map[key].endTime)) map[key].endTime = endTime;
    return map[key];
  }

  function addModel(day, name, requests, input, cached, output, total) {
    if (!day.models[name]) {
      day.models[name] = {
        modelName: name,
        requestCount: 0,
        inputTokens: 0,
        cacheReadTokens: 0,
        outputTokens: 0,
        totalTokens: 0,
      };
    }
    var model = day.models[name];
    model.requestCount += requests;
    model.inputTokens += input;
    model.cacheReadTokens += cached;
    model.outputTokens += output;
    model.totalTokens += total;
  }

  function makeAdminDaily(costs, completions) {
    var map = {};
    for (var i = 0; i < costs.length; i++) {
      var costBucket = costs[i] || {};
      var start = intValue(costBucket.start_time);
      if (!start) continue;
      var day = dailyBucket(map, start, intValue(costBucket.end_time));
      var results = Array.isArray(costBucket.results) ? costBucket.results : [];
      for (var j = 0; j < results.length; j++) {
        var result = results[j] || {};
        var amount = result.amount && typeof result.amount === "object"
          ? numberValue(result.amount.value)
          : numberValue(result.amount);
        amount = amount === null ? 0 : amount;
        day.costUSD += amount;
        var item = displayName(result.line_item, "API");
        day.lineItems[item] = (day.lineItems[item] || 0) + amount;
      }
    }

    for (var c = 0; c < completions.length; c++) {
      var usageBucket = completions[c] || {};
      var usageStart = intValue(usageBucket.start_time);
      if (!usageStart) continue;
      var usageDay = dailyBucket(map, usageStart, intValue(usageBucket.end_time));
      var usageResults = Array.isArray(usageBucket.results) ? usageBucket.results : [];
      for (var k = 0; k < usageResults.length; k++) {
        var row = usageResults[k] || {};
        var input = intValue(row.input_tokens) + intValue(row.input_audio_tokens);
        var cached = intValue(row.input_cached_tokens);
        var output = intValue(row.output_tokens) + intValue(row.output_audio_tokens);
        var requests = intValue(row.num_model_requests);
        var total = input + output;
        usageDay.requests += requests;
        usageDay.inputTokens += input;
        usageDay.cachedInputTokens += cached;
        usageDay.outputTokens += output;
        usageDay.totalTokens += total;
        addModel(
          usageDay,
          displayName(row.model, "Responses and Chat Completions"),
          requests,
          input,
          cached,
          output,
          total
        );
      }
    }

    return Object.keys(map)
      .sort()
      .map(function (key) {
        var day = map[key];
        var modelBreakdowns = Object.keys(day.models)
          .map(function (name) { return day.models[name]; })
          .sort(function (a, b) {
            return b.totalTokens - a.totalTokens || a.modelName.localeCompare(b.modelName);
          });
        var lineItems = Object.keys(day.lineItems)
          .map(function (name) { return { name: name, costUSD: day.lineItems[name] }; })
          .sort(function (a, b) {
            return b.costUSD - a.costUSD || a.name.localeCompare(b.name);
          });
        return {
          date: day.date,
          inputTokens: day.inputTokens,
          outputTokens: day.outputTokens,
          cacheReadTokens: day.cachedInputTokens,
          totalTokens: day.totalTokens,
          requestCount: day.requests,
          costUSD: day.costUSD,
          totalCost: day.costUSD,
          modelsUsed: modelBreakdowns.map(function (model) { return model.modelName; }),
          modelBreakdowns: modelBreakdowns,
          lineItems: lineItems,
        };
      });
  }

  function summarize(daily) {
    var summary = { costUSD: 0, tokens: 0, requests: 0 };
    for (var i = 0; i < daily.length; i++) {
      summary.costUSD += Number(daily[i].costUSD) || 0;
      summary.tokens += Number(daily[i].totalTokens) || 0;
      summary.requests += Number(daily[i].requestCount) || 0;
    }
    return summary;
  }

  function findDay(daily, key) {
    for (var i = 0; i < daily.length; i++) {
      if (daily[i].date === key) return daily[i];
    }
    return null;
  }

  function usageLabel(summary) {
    var parts = [formatUsd(summary.costUSD)];
    parts.push(fmtCount(summary.tokens, "tokens"));
    parts.push(fmtCount(summary.requests, "requests"));
    return parts.join(" · ");
  }

  function pushDayLine(lines, ctx, label, day) {
    lines.push(ctx.line.text({
      label: label,
      value: usageLabel(day ? summarize([day]) : { costUSD: 0, tokens: 0, requests: 0 }),
    }));
  }

  function collectUsageChartPoints(daily) {
    var hasCost = daily.some(function (day) { return Number(day.costUSD) > 0; });
    var points = [];
    for (var i = 0; i < daily.length; i++) {
      var day = daily[i];
      var cost = Number(day.costUSD) || 0;
      var tokens = Number(day.totalTokens) || 0;
      var requests = Number(day.requestCount) || 0;
      var value = hasCost ? cost : tokens;
      if (value < 0) continue;
      points.push({
        label: shortDayLabel(day.date),
        value: value,
        valueLabel: hasCost
          ? formatUsd(cost) + " · " + fmtCount(tokens, "tokens")
          : fmtCount(tokens, "tokens") + " · " + fmtCount(requests, "requests"),
      });
    }
    return points;
  }

  function persistDaily(ctx, daily) {
    if (!ctx.host.usageDaily || typeof ctx.host.usageDaily.ingest !== "function") return;
    if (!daily || !daily.length) return;
    try {
      ctx.host.usageDaily.ingest({
        displayName: "OpenAI API",
        source: "admin_billing",
        daily: daily,
      });
    } catch (_) {}
  }

  function topModelText(daily) {
    var totals = {};
    for (var i = 0; i < daily.length; i++) {
      var models = Array.isArray(daily[i].modelBreakdowns) ? daily[i].modelBreakdowns : [];
      for (var j = 0; j < models.length; j++) {
        var model = models[j];
        if (!model || !model.modelName) continue;
        totals[model.modelName] = (totals[model.modelName] || 0) + (Number(model.totalTokens) || 0);
      }
    }
    var ranked = Object.keys(totals)
      .map(function (name) { return { name: name, tokens: totals[name] }; })
      .sort(function (a, b) { return b.tokens - a.tokens || a.name.localeCompare(b.name); });
    if (!ranked.length || ranked[0].tokens <= 0) return null;
    return ranked[0].name + " · " + fmtCount(ranked[0].tokens, "tokens");
  }

  function fetchAdminUsage(ctx, credential) {
    var costs = fetchPagedBuckets(ctx, {
      name: "costs",
      url: ORG_COSTS_URL,
      groupBy: "line_item",
      apiKey: credential.apiKey,
      projectId: credential.projectId,
    });
    var completions = fetchPagedBuckets(ctx, {
      name: "completions",
      url: ORG_COMPLETIONS_URL,
      groupBy: "model",
      apiKey: credential.apiKey,
      projectId: credential.projectId,
    });
    var daily = makeAdminDaily(costs, completions);
    var total = summarize(daily);
    var now = new Date();
    var todayKey = dayKeyFromUtcMs(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()));
    var yesterdayKey = dayKeyFromUtcMs(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()) - 24 * 60 * 60 * 1000);

    var lines = [
      ctx.line.text({ label: "Spend", value: formatUsd(total.costUSD) }),
      ctx.line.text({ label: "Requests", value: fmtCount(total.requests, "requests") }),
      ctx.line.text({ label: "Tokens", value: fmtCount(total.tokens, "tokens") }),
    ];
    pushDayLine(lines, ctx, "Today", findDay(daily, todayKey));
    pushDayLine(lines, ctx, "Yesterday", findDay(daily, yesterdayKey));
    lines.push(ctx.line.text({ label: "Last 30 Days", value: usageLabel(total) }));

    var topModel = topModelText(daily);
    if (topModel) lines.push(ctx.line.text({ label: "Top Model", value: topModel }));

    var points = collectUsageChartPoints(daily);
    if (points.length) {
      lines.push(ctx.line.barChart({
        label: "Usage Trend",
        points: points,
        note: credential.projectId
          ? "OpenAI Admin API usage for project " + credential.projectId + "."
          : "OpenAI Admin API organization usage.",
        color: "#10a37f",
      }));
    }

    persistDaily(ctx, daily);
    return { lines: lines };
  }

  // Only the legacy, organization-wide balance is scoped by OpenAI-Organization;
  // Admin cost and usage requests keep their own credential scope.
  function fetchLegacyBalance(ctx, apiKey) {
    var organizationId = loadOrganizationId(ctx);
    var json = requestJson(ctx, CREDIT_GRANTS_URL, apiKey, "credit grants",
      organizationId ? { "OpenAI-Organization": organizationId } : null);
    var granted = typeof json.total_granted === "number" ? json.total_granted : 0;
    var used = typeof json.total_used === "number" ? json.total_used : 0;
    var available = typeof json.total_available === "number" ? json.total_available : Math.max(0, granted - used);
    var usedPct = granted > 0 ? Math.min(100, (used / granted) * 100) : (available > 0 ? 0 : 100);

    var expiresAt = null;
    var grants = json.grants && Array.isArray(json.grants.data) ? json.grants.data : [];
    var now = Date.now();
    for (var i = 0; i < grants.length; i++) {
      var ts = grants[i].expires_at;
      if (typeof ts === "number" && ts * 1000 > now) {
        if (expiresAt === null || ts < expiresAt) expiresAt = ts;
      }
    }

    var opts = {
      label: "Credits",
      used: usedPct,
      limit: 100,
      format: { kind: "percent" },
    };
    if (expiresAt !== null) opts.resetsAt = ctx.util.toIso(expiresAt * 1000);

    return {
      lines: [
        ctx.line.progress(opts),
        ctx.line.text({ label: "Available", value: formatUsd(available) }),
      ],
    };
  }

  function probe(ctx) {
    var credential = loadCredential(ctx);
    if (!credential) {
      throw "OpenAI API key not found. Set OPENAI_ADMIN_KEY or OPENAI_API_KEY.";
    }

    var adminError = null;
    try {
      return fetchAdminUsage(ctx, credential);
    } catch (e) {
      adminError = e;
      ctx.host.log.warn("OpenAI Admin API usage failed: " + (e && e.message ? e.message : String(e)));
    }

    if (credential.projectId && credential.usesAdminKey) {
      throw adminError && adminError.message ? adminError.message : String(adminError);
    }

    try {
      return fetchLegacyBalance(ctx, credential.apiKey);
    } catch (legacyError) {
      if (adminError && !adminError.authRejected) {
        throw adminError.message || String(adminError);
      }
      throw legacyError && legacyError.message ? legacyError.message : String(legacyError);
    }
  }

  globalThis.__openusage_plugin = { id: "openai-api", probe: probe };
})();
