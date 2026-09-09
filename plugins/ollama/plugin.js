(function () {
  var SETTINGS_URL = "https://ollama.com/settings";
  var TAGS_URL = "https://ollama.com/api/tags";
  var VALIDATION_URL = "https://ollama.com/api/web_search";
  var SESSION_COOKIE = "__Secure-session";
  var SESSION_MS = 5 * 60 * 60 * 1000;
  var WEEK_MS = 7 * 24 * 60 * 60 * 1000;

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

  function getApiKey(ctx) {
    return clean(ctx.provider && ctx.provider.apiKey) ||
      setting(ctx, ["apiKey", "ollamaApiKey"]) ||
      env(ctx, "OLLAMA_API_KEY") ||
      env(ctx, "OLLAMA_KEY");
  }

  function getCookieHeader(ctx, optional) {
    var raw = clean(ctx.provider && ctx.provider.cookieHeader) ||
      setting(ctx, ["cookieHeader", "cookie"]) ||
      env(ctx, "OLLAMA_COOKIE");
    if (!raw) {
      if (optional) return null;
      throw "Set OLLAMA_COOKIE to your " + SESSION_COOKIE + " cookie value from ollama.com/settings.";
    }
    var val = raw;
    if (val.toLowerCase().slice(0, 5) === "curl ") {
      var cookieMatch = val.match(/(?:^|\s)-H\s+['"]Cookie:\s*([^'"]+)['"]/i) ||
        val.match(/(?:^|\s)--header\s+['"]Cookie:\s*([^'"]+)['"]/i);
      if (cookieMatch) val = cookieMatch[1].trim();
    }
    if (val.toLowerCase().slice(0, 7) === "cookie:") {
      val = val.slice(7).trim();
    }
    if (val.indexOf("=") < 0) {
      return SESSION_COOKIE + "=" + val;
    }
    return val;
  }

  function request(ctx, req) {
    try {
      return ctx.host.http.request(req);
    } catch (e) {
      ctx.host.log.warn("Ollama request failed: " + String(e));
      throw "Ollama request failed. Check your connection.";
    }
  }

  function parseJson(ctx, text) {
    var parsed = ctx.util && typeof ctx.util.tryParseJson === "function"
      ? ctx.util.tryParseJson(text)
      : null;
    if (!parsed && text) {
      try {
        parsed = JSON.parse(text);
      } catch (_) {}
    }
    return parsed;
  }

  function probeApi(ctx, apiKey) {
    var validation = request(ctx, {
      method: "POST",
      url: VALIDATION_URL,
      headers: {
        Authorization: "Bearer " + apiKey,
        Accept: "application/json",
        "Content-Type": "application/json",
        "User-Agent": "usagestat/" + String(ctx.app && ctx.app.version ? ctx.app.version : "0.0.0"),
      },
      bodyText: '{"query":""}',
      timeoutMs: 15000,
    });
    if (validation.status === 401 || validation.status === 403) {
      throw "Ollama API key is invalid or revoked.";
    }
    if (validation.status !== 200 && validation.status !== 400) {
      throw "Ollama API validation returned HTTP " + validation.status + ".";
    }

    var tags = request(ctx, {
      method: "GET",
      url: TAGS_URL,
      headers: {
        Authorization: "Bearer " + apiKey,
        Accept: "application/json",
        "User-Agent": "usagestat/" + String(ctx.app && ctx.app.version ? ctx.app.version : "0.0.0"),
      },
      timeoutMs: 15000,
    });
    if (tags.status === 401 || tags.status === 403) {
      throw "Ollama API key is invalid or revoked.";
    }
    if (tags.status < 200 || tags.status >= 300) {
      throw "Ollama API returned HTTP " + tags.status + ".";
    }
    var parsed = parseJson(ctx, tags.bodyText);
    if (!parsed || !Array.isArray(parsed.models)) {
      throw "Ollama API tags response was not valid JSON.";
    }

    return {
      displayName: "Ollama",
      source: "api",
      plan: "API key",
      lines: [
        ctx.line.text({
          label: "Cloud Models",
          value: String(parsed.models.length) + " available",
        }),
        ctx.line.badge({
          label: "Status",
          text: "API key valid; quota unavailable",
          color: "#22c55e",
        }),
      ],
    };
  }

  function blockEnd(html, start, currentLabel) {
    var labels = ["Monthly usage", "Session usage", "Hourly usage", "Weekly usage"];
    var end = html.length;
    for (var i = 0; i < labels.length; i++) {
      if (labels[i] === currentLabel) continue;
      var idx = html.indexOf(labels[i], start + currentLabel.length);
      if (idx >= 0 && idx < end) end = idx;
    }
    return Math.min(end, start + 4000);
  }

  function stripHtmlEntities(value) {
    return String(value || "")
      .replace(/&nbsp;/g, " ")
      .replace(/&amp;/g, "&")
      .replace(/&#x2F;/g, "/")
      .replace(/\s+/g, " ")
      .trim();
  }

  function parseIso(segment) {
    var match = String(segment || "").match(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})/);
    if (!match) return null;
    var ms = Date.parse(match[0]);
    return Number.isFinite(ms) ? new Date(ms).toISOString() : null;
  }

  function parseResetDescription(segment) {
    var match = String(segment || "").match(/(resets?\s+in\s+[^<\n\r]+|reset\s+[^<\n\r]+)/i);
    return match ? stripHtmlEntities(match[1]) : null;
  }

  function parseUsageBlock(html, labels, periodMs) {
    for (var i = 0; i < labels.length; i++) {
      var label = labels[i];
      var idx = html.indexOf(label);
      if (idx < 0) continue;
      var segment = html.slice(idx, blockEnd(html, idx, label));
      var usedMatch = segment.match(/(\d+(?:\.\d+)?)\s*%\s*used/i);
      var dollars = segment.match(/\$((?:\d{1,3}(?:,\d{3})+|\d+)(?:\.\d+)?)\s+of\s+\$((?:\d{1,3}(?:,\d{3})+|\d+)(?:\.\d+)?)\s+used/i);
      var widthMatch = segment.match(/width:\s*(\d+(?:\.\d+)?)%/i);
      var value = usedMatch ? parseFloat(usedMatch[1]) : null;
      if (value === null && dollars) {
        var spent = Number(dollars[1].replace(/,/g, ""));
        var cap = Number(dollars[2].replace(/,/g, ""));
        if (Number.isFinite(spent) && Number.isFinite(cap) && cap > 0) value = spent / cap * 100;
      }
      if (!Number.isFinite(value) && widthMatch) value = parseFloat(widthMatch[1]);
      if (Number.isFinite(value)) {
        return {
          used: Math.max(0, Math.min(100, value)),
          resetsAt: parseIso(segment),
          detail: parseResetDescription(segment),
          periodMs: label === "Hourly usage" ? null : periodMs,
        };
      }
    }
    return null;
  }

  function progress(ctx, label, block) {
    var line = {
      label: label,
      used: block.used,
      limit: 100,
      format: { kind: "percent" },
    };
    if (block.periodMs) line.periodDurationMs = block.periodMs;
    if (block.resetsAt) line.resetsAt = block.resetsAt;
    if (block.detail) line.detail = block.detail;
    return ctx.line.progress(line);
  }

  function probeWeb(ctx) {
    var cookieHeader = getCookieHeader(ctx);
    var resp = request(ctx, {
      method: "GET",
      url: SETTINGS_URL,
      headers: {
        Cookie: cookieHeader,
        Accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/143.0.0.0 Safari/537.36",
      },
      timeoutMs: 20000,
    });

    if (resp.status === 401 || resp.status === 403) {
      throw "Session expired. Update OLLAMA_COOKIE with a fresh session token.";
    }

    var html = resp.bodyText || "";
    if (!html.includes("Monthly usage") && !html.includes("Session usage") && !html.includes("Weekly usage") && !html.includes("Cloud Usage")) {
      if (html.includes("Sign in") || resp.status !== 200) {
        throw "Session expired. Update OLLAMA_COOKIE with a fresh session token.";
      }
    }

    var monthly = parseUsageBlock(html, ["Monthly usage"], null);
    if (monthly && monthly.resetsAt) monthly.periodMs = ctx.util.calendarMonthDuration(monthly.resetsAt);
    var session = parseUsageBlock(html, ["Session usage", "Hourly usage"], SESSION_MS);
    var weekly = parseUsageBlock(html, ["Weekly usage"], WEEK_MS);

    if (!monthly && !session && !weekly) {
      throw "Could not find usage data on Ollama settings page.";
    }

    var lines = [];
    if (monthly) lines.push(progress(ctx, "Monthly credits", monthly));
    if (session) lines.push(progress(ctx, "Session", session));
    if (weekly) lines.push(progress(ctx, "Weekly", weekly));

    var planMatch = html.match(/(?:Included usage|Cloud Usage)\s*<\/span>\s*<span[^>]*>([^<]+)<\/span/i);
    return {
      displayName: "Ollama",
      source: "web",
      plan: planMatch ? stripHtmlEntities(planMatch[1]) : undefined,
      lines: lines,
    };
  }

  function probe(ctx) {
    var source = String(ctx.sourceMode || "auto").toLowerCase();
    var apiKey = getApiKey(ctx);
    if (source === "api") {
      if (!apiKey) throw "Missing Ollama API key. Set OLLAMA_API_KEY.";
      return probeApi(ctx, apiKey);
    }
    if (source === "web") return probeWeb(ctx);
    if (getCookieHeader(ctx, true)) {
      try {
        return probeWeb(ctx);
      } catch (e) {
        if (!apiKey) throw e;
        ctx.host.log.warn("Ollama settings unavailable; checking API access.");
      }
    }
    if (apiKey) {
      return probeApi(ctx, apiKey);
    }
    return probeWeb(ctx);
  }

  globalThis.__openusage_plugin = { id: "ollama", probe: probe };
})();
