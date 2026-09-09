(function () {
  var BILLING_URL = "https://console.sakana.ai/billing";
  var USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

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

  function cookieHeader(ctx) {
    return normalizeCookie(ctx.provider && ctx.provider.cookieHeader) ||
      normalizeCookie(setting(ctx, ["cookieHeader", "cookie"])) ||
      normalizeCookie(env(ctx, "SAKANA_COOKIE"));
  }

  function looksSignedOut(text) {
    var lower = String(text || "").toLowerCase();
    return lower.indexOf("sign in") >= 0 || lower.indexOf("log in") >= 0 || lower.indexOf("/auth/") >= 0;
  }

  function parseReset(raw) {
    var text = String(raw || "").replace(/\bat\b/i, "").replace(/\s+/g, " ").trim();
    var ms = Date.parse(text + " UTC");
    return Number.isFinite(ms) ? new Date(ms).toISOString() : null;
  }

  function extractResetText(segment) {
    var patterns = [
      /([A-Z][a-z]+ \d{1,2}, \d{4} at \d{1,2}:\d{2} [AP]M)/i,
      /(?:reset|renews?)[^A-Z]{0,80}([A-Z][a-z]+ \d{1,2}, \d{4} at \d{1,2}:\d{2} [AP]M)/i,
    ];
    for (var i = 0; i < patterns.length; i++) {
      var match = patterns[i].exec(segment);
      if (match && match[1]) return match[1];
    }
    return null;
  }

  function extractPercent(segment) {
    var patterns = [
      /([0-9]+(?:\.[0-9]+)?)\s*%\s*(?:used|usage)?/i,
      /(?:used|usage)[^0-9]{0,40}([0-9]+(?:\.[0-9]+)?)\s*%/i,
      /"(?:usedPercent|used_percent|percent)"\s*:\s*([0-9]+(?:\.[0-9]+)?)/i,
    ];
    for (var i = 0; i < patterns.length; i++) {
      var match = patterns[i].exec(segment);
      if (match && match[1]) {
        var value = Number(match[1]);
        if (Number.isFinite(value)) return value;
      }
    }
    return null;
  }

  function extractWindow(ctx, text, labels, displayLabel, minutes) {
    var lower = text.toLowerCase();
    var index = -1;
    for (var i = 0; i < labels.length; i++) {
      var found = lower.indexOf(labels[i]);
      if (found >= 0 && (index < 0 || found < index)) index = found;
    }
    if (index < 0) return null;
    var end = Math.min(text.length, index + 1400);
    var boundaries = displayLabel === "Weekly" ? ["5-hour", "5 hour", "five-hour", "session"] : ["weekly", "week"];
    for (var b = 0; b < boundaries.length; b++) {
      var next = lower.indexOf(boundaries[b], index + 1);
      if (next >= 0) end = Math.min(end, next);
    }
    var segment = text.slice(index, end);
    var percent = extractPercent(segment);
    if (percent === null) return null;
    var resetText = extractResetText(segment);
    var opts = {
      label: displayLabel,
      used: Math.max(0, Math.min(100, percent)),
      limit: 100,
      format: { kind: "percent" },
      periodDurationMs: minutes * 60 * 1000,
    };
    if (resetText) {
      opts.resetsAt = parseReset(resetText);
      opts.detail = resetText;
    }
    return ctx.line.progress(opts);
  }

  function probe(ctx) {
    var cookie = cookieHeader(ctx);
    if (!cookie) throw "Sakana session not configured. Set SAKANA_COOKIE or provider cookieHeader.";
    var resp = ctx.util.request({
      method: "GET",
      url: BILLING_URL,
      headers: {
        Cookie: cookie,
        Accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        "User-Agent": USER_AGENT,
      },
      timeoutMs: 15000,
    });
    if (ctx.util.isAuthStatus(resp.status)) throw "Sakana session expired.";
    if (resp.status < 200 || resp.status >= 300) throw "Sakana billing returned HTTP " + resp.status + ".";
    var primary = extractWindow(ctx, resp.bodyText || "", ["5-hour", "5 hour", "five-hour", "session"], "5-hour", 5 * 60);
    var lines = primary ? [primary] : [];
    var weekly = extractWindow(ctx, resp.bodyText || "", ["weekly", "week"], "Weekly", 7 * 24 * 60);
    if (weekly) lines.push(weekly);
    if (!lines.length) throw looksSignedOut(resp.bodyText) ? "Sakana login required." : "Missing Sakana quota windows.";
    try {
      var payg = ctx.util.request({ method: "GET", url: BILLING_URL + "?tab=payAsYouGo",
        headers: { Cookie: cookie, Accept: "text/html,application/xhtml+xml", "User-Agent": USER_AGENT }, timeoutMs: 1500 });
      if (payg.status === 200) {
        var html = String(payg.bodyText || "").replace(/<!--[\s\S]*?-->/g, "");
        var balance = /<h2[^>]*>\s*Credit balance\s*<\/h2>[\s\S]{0,900}?<p[^>]*tabular-nums[^>]*>\s*\$?([0-9][0-9,]*(?:\.[0-9]+)?)\s*<\/p>/i.exec(html);
        var spent = /<h2[^>]*>\s*Usage\s*<\/h2>\s*<span[^>]*>\s*Total:\s*\$?([0-9][0-9,]*(?:\.[0-9]+)?)\s*<\/span>/i.exec(html);
        if (balance && Number.isFinite(Number(balance[1].replace(/,/g, "")))) lines.push(ctx.line.text({ label: "PAYG Balance", value: "$" + Number(balance[1].replace(/,/g, "")).toFixed(2) }));
        if (balance && spent && Number.isFinite(Number(spent[1].replace(/,/g, "")))) lines.push(ctx.line.text({ label: "PAYG Usage", value: "$" + Number(spent[1].replace(/,/g, "")).toFixed(2) }));
      }
    } catch (_) {
      ctx.host.log.warn("Sakana PAYG balance unavailable");
    }
    return { displayName: "Sakana AI", source: "web", plan: "Sakana Console", lines: lines };
  }

  globalThis.__openusage_plugin = { id: "sakana", probe: probe };
})();
