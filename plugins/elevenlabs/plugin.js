(function () {
  function apiKey(ctx) {
    return (ctx.provider && ctx.provider.apiKey) || ctx.host.env.get("ELEVENLABS_API_KEY") || ctx.host.env.get("XI_API_KEY");
  }

  function apiBase(ctx) {
    return ctx.host.env.get("ELEVENLABS_API_URL") || "https://api.elevenlabs.io";
  }

  function authError(status, json) {
    var detail = json && json.detail;
    var codes = detail && typeof detail === "object" ? [detail.code, detail.status] : [];
    for (var i = 0; i < codes.length; i++) {
      var code = typeof codes[i] === "string" ? codes[i].trim().toLowerCase() : "";
      if (code === "invalid_api_key") return "ElevenLabs rejected the API key. Check whether it is valid or revoked.";
      if (code === "missing_permissions" || code === "insufficient_permissions") {
        return "ElevenLabs API key needs the user_read permission to fetch subscription usage.";
      }
    }
    return status === 403
      ? "ElevenLabs denied access. Check the API key's endpoint permissions and IP allowlist."
      : "ElevenLabs could not authenticate the API key. Check the key and its permissions.";
  }

  function probe(ctx) {
    var key = apiKey(ctx);
    if (!key) throw "ElevenLabs API key not found. Set ELEVENLABS_API_KEY or XI_API_KEY.";

    var result = ctx.util.requestJson({
      method: "GET",
      url: apiBase(ctx).replace(/\/$/, "") + "/v1/user/subscription",
      headers: { "xi-api-key": key, Accept: "application/json" },
      timeoutMs: 15000,
    });
    if (ctx.util.isAuthStatus(result.resp.status)) throw authError(result.resp.status, result.json);
    if (result.resp.status < 200 || result.resp.status >= 300) throw "ElevenLabs API error (HTTP " + result.resp.status + ").";
    if (!result.json) throw "Could not parse ElevenLabs subscription response.";

    var sub = result.json;
    var used = Number(sub.character_count || 0);
    var limit = Number(sub.character_limit || 0);
    var lines = [];
    var opts = {
      label: "Characters",
      used: used,
      limit: limit > 0 ? limit : Math.max(used, 1),
      format: { kind: "count", suffix: "chars" },
    };
    if (sub.next_character_count_reset_unix) opts.resetsAt = ctx.util.toIso(Number(sub.next_character_count_reset_unix) * 1000);
    lines.push(ctx.line.progress(opts));

    var voiceUsed = Number(sub.voice_count || 0);
    var voiceLimit = Number(sub.voice_limit || sub.professional_voice_limit || 0);
    if (voiceLimit > 0 || voiceUsed > 0) {
      lines.push(ctx.line.text({ label: "Voice slots", value: voiceUsed + " / " + voiceLimit }));
    }
    if (sub.tier) lines.push(ctx.line.badge({ label: "Plan", text: String(sub.tier), color: "#22c55e" }));

    return { lines: lines };
  }

  globalThis.__openusage_plugin = { id: "elevenlabs", probe: probe };
})();
