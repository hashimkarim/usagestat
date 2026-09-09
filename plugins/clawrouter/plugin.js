(function () {
  function clean(value) { return typeof value === "string" && value.trim() ? value.trim() : null }
  function integer(value) {
    if (!Number.isSafeInteger(value) || value < 0) throw "Invalid ClawRouter usage counters."
    return value
  }
  function money(value) { return "$" + (integer(value) / 1000000).toFixed(6) }
  function probe(ctx) {
    const provider = ctx.provider || {}
    const settings = provider.settings || {}
    const key = clean(provider.apiKey) || clean(ctx.host.env.get("CLAWROUTER_API_KEY"))
    if (!key) throw "Set CLAWROUTER_API_KEY or the ClawRouter provider API key."
    let base = ctx.host.http.validateBaseUrl(clean(settings.baseUrl) || clean(ctx.host.env.get("CLAWROUTER_BASE_URL")) || "https://clawrouter.openclaw.ai", false)
    if (!base.endsWith("/v1")) base += "/v1"
    const result = ctx.util.requestJson({ method: "GET", url: base + "/usage",
      headers: { Authorization: "Bearer " + key, Accept: "application/json" }, timeoutMs: 10000 })
    if (ctx.util.isAuthStatus(result.resp.status)) throw "ClawRouter rejected the API key."
    if (result.resp.status < 200 || result.resp.status >= 300) throw "ClawRouter returned HTTP " + result.resp.status + "."
    const data = result.json
    if (!data || !data.budget || !data.usage || !data.usage.summary || !Array.isArray(data.usage.providers)) throw "Invalid ClawRouter usage response."
    const budget = data.budget
    const summary = data.usage.summary
    const lines = [
      ctx.line.text({ label: "Requests", value: String(integer(summary.requestCount)),
        subtitle: integer(summary.successCount) + " succeeded; " + integer(summary.errorCount) + " failed" }),
      ctx.line.text({ label: "Tokens", value: String(integer(summary.totalTokens)),
        subtitle: integer(summary.inputTokens) + " input; " + integer(summary.outputTokens) + " output" }),
      ctx.line.text({ label: "Actual cost", value: money(summary.actualCostMicros) }),
    ]
    if (budget.spentMicros !== null && budget.spentMicros !== undefined && budget.limitMicros !== null && budget.limitMicros !== undefined) {
      const month = typeof budget.windowKey === "string" && budget.windowKey.match(/(\d{4})-(0[1-9]|1[0-2])$/)
      lines.unshift(ctx.line.progress({ label: "Monthly budget", used: integer(budget.spentMicros) / 1000000,
        limit: integer(budget.limitMicros) / 1000000, format: { kind: "dollars" },
        resetsAt: month ? new Date(Date.UTC(Number(month[1]), Number(month[2]), 1)).toISOString() : undefined,
        periodDurationMs: 30 * 24 * 60 * 60 * 1000 }))
    }
    const routes = data.usage.providers.map((row) => {
      if (!row || !clean(row.provider)) throw "Invalid ClawRouter routed provider."
      return { label: row.provider, value: integer(row.actualCostMicros) / 1000000, valueLabel: money(row.actualCostMicros) }
    }).sort((a, b) => b.value - a.value)
    if (routes.length) lines.push(ctx.line.barChart({ label: "Routed provider costs", points: routes.slice(0, 20) }))
    return { displayName: "ClawRouter", source: "api", plan: budget.configured ? "Monthly budget" : "Unmetered", lines }
  }
  globalThis.__openusage_plugin = { id: "clawrouter", probe }
})()
