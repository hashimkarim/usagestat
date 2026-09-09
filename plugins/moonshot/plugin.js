(function () {
  function apiKey(ctx) {
    return (ctx.provider && ctx.provider.apiKey) || ctx.host.env.get("MOONSHOT_API_KEY") || ctx.host.env.get("MOONSHOT_KEY");
  }

  function baseUrl(ctx) {
    var region = String((ctx.provider && ctx.provider.region) || ctx.host.env.get("MOONSHOT_REGION") || "").toLowerCase();
    var explicit = ctx.host.env.get("MOONSHOT_API_URL");
    if (explicit) return explicit.replace(/\/$/, "");
    return region === "cn" || region === "china" ? "https://api.moonshot.cn" : "https://api.moonshot.ai";
  }

  function pick(obj, names) {
    for (var i = 0; i < names.length; i++) {
      var v = obj && obj[names[i]];
      if (typeof v === "number" && Number.isFinite(v)) return v;
      if (typeof v === "string" && v.trim() !== "" && Number.isFinite(Number(v))) return Number(v);
    }
    return null;
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "Moonshot API key not found. Set MOONSHOT_API_KEY or MOONSHOT_KEY.";

    var base = baseUrl(ctx);
    var result = ctx.util.requestJson({
      method: "GET",
      url: base + "/v1/users/me/balance",
      headers: { Authorization: "Bearer " + key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    if (ctx.util.isAuthStatus(result.resp.status)) throw "Moonshot API key invalid or expired.";
    if (result.resp.status < 200 || result.resp.status >= 300) throw "Moonshot API error (HTTP " + result.resp.status + ").";

    var json = result.json || {};
    var data = json.data || json;
    var available = pick(data, ["available_balance", "availableBalance", "balance", "cash_balance"]);
    if (available === null) throw "Moonshot response did not include a recognizable balance.";
    var currency = /^https:\/\/api\.moonshot\.cn(?::443)?(?:\/|$)/i.test(base) ? "CNY" : "USD";
    return { source: "api", lines: [ctx.line.text({ label: "Balance", value: currency + " " + available.toFixed(2) })] };
  }

  globalThis.__openusage_plugin = { id: "moonshot", probe: probe };
})();
