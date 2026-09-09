(function () {
  function clean(value) { return typeof value === "string" && value.trim() ? value.trim() : null }
  function number(value) {
    if (typeof value !== "number" || !Number.isFinite(value) || value < 0) throw "Invalid Wayfinder usage response."
    return value
  }
  function probe(ctx) {
    const settings = ctx.provider && ctx.provider.settings || {}
    const base = ctx.host.http.validateBaseUrl(clean(settings.baseUrl) || clean(ctx.host.env.get("WAYFINDER_GATEWAY_URL")) || "http://127.0.0.1:8088", true)
    function get(path, json) {
      const resp = ctx.host.http.request({ method: "GET", url: base + path, timeoutMs: 5000 })
      if (resp.status < 200 || resp.status >= 300) throw "Wayfinder gateway returned HTTP " + resp.status + "."
      if (!json) return resp.bodyText
      const data = ctx.util.tryParseJson(resp.bodyText)
      if (!data || typeof data !== "object") throw "Invalid Wayfinder response."
      return data
    }
    const health = get("/healthz", true)
    const models = get("/router/models", true)
    const savings = get("/v1/savings?period=30d", true)
    if (typeof health.status !== "string" || !Array.isArray(models.models) || typeof savings.priced !== "boolean") throw "Incomplete Wayfinder response."
    const lines = [
      ctx.line.badge({ label: "Gateway", text: health.offline ? "Offline mode" : models.dry_run ? "Dry run" : health.status }),
      ctx.line.text({ label: "Models", value: String(models.models.length) }),
      ctx.line.text({ label: "Requests", value: String(number(savings.requests)), subtitle: "Last 30 days" }),
      ctx.line.text({ label: "Tokens", value: String(number(savings.tokens)) }),
    ]
    if (number(savings.saved) > 0) {
      lines.push(ctx.line.text({ label: "Saved", value: (savings.priced ? "$" + savings.saved.toFixed(4) + "; " : "") +
        number(savings.saved_pct).toFixed(1) + "%", subtitle: "Compared with highest-cost route" }))
    }
    const routes = Object.keys(savings.by_route || {}).map((name) => ({
      label: name, value: number(savings.by_route[name].requests),
    })).sort((a, b) => b.value - a.value)
    if (routes.length) lines.push(ctx.line.barChart({ label: "Routed requests", points: routes.slice(0, 20) }))
    try {
      const metrics = get("/metrics", false)
      const sum = metrics.match(/^wayfinder_router_decision_latency_seconds_sum\s+([0-9.eE+-]+)(?:\s|$)/m)
      const count = metrics.match(/^wayfinder_router_decision_latency_seconds_count\s+([0-9.eE+-]+)(?:\s|$)/m)
      if (sum && count && number(Number(count[1])) > 0) {
        lines.push(ctx.line.text({ label: "Avg decision", value: (number(Number(sum[1])) / Number(count[1]) * 1000).toFixed(1) + " ms" }))
      }
    } catch (_) { ctx.host.log.warn("Wayfinder latency unavailable") }
    return { displayName: "Wayfinder", source: "local", lines }
  }
  globalThis.__openusage_plugin = { id: "wayfinder", probe }
})()
