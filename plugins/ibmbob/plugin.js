(function () {
  function clean(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null
  }
  function number(value) {
    if (typeof value !== "number" && typeof value !== "string") return null
    if (typeof value === "string" && !value.trim()) return null
    const n = Number(value)
    return Number.isFinite(n) && n >= 0 ? n : null
  }
  function regionalBase(domain) {
    if (!clean(domain)) return "https://api.us-east.bob.ibm.com"
    const host = domain.trim().toLowerCase()
    if (!/^(?:[a-z0-9-]+\.)*bob\.ibm\.com$/.test(host)) throw "IBM Bob returned an untrusted regional host."
    return "https://" + (host.startsWith("api.") ? host : "api." + host)
  }
  function request(ctx, url, headers) {
    const result = ctx.util.requestJson({ method: "GET", url, headers, timeoutMs: 5000 })
    if (ctx.util.isAuthStatus(result.resp.status)) throw "IBM Bob rejected the API key."
    if (result.resp.status < 200 || result.resp.status >= 300) throw "IBM Bob API returned HTTP " + result.resp.status + "."
    if (!result.json || typeof result.json !== "object") throw "Invalid IBM Bob response."
    return result.json
  }
  function probe(ctx) {
    const key = clean(ctx.provider && ctx.provider.apiKey) || clean(ctx.host.env.get("BOBSHELL_API_KEY"))
    if (!key) throw "Set BOBSHELL_API_KEY or the IBM Bob provider API key."
    const jwt = key.split(".").length === 3 && ctx.jwt.decodePayload(key)
    const headers = { Authorization: (jwt ? "Bearer " : "Apikey ") + key, Accept: "application/json", "User-Agent": "UsageStat" }
    const profile = request(ctx, "https://api.us-east.bob.ibm.com/admin/v1/profile", headers)
    if (!Array.isArray(profile.instances)) throw "IBM Bob profile is missing subscriptions."
    const lines = []
    const plans = []
    let count = 0
    for (const instance of profile.instances) {
      if (!instance || !clean(instance.user_id) || !Array.isArray(instance.teams)) continue
      const base = regionalBase(instance.region_domain)
      for (const team of instance.teams) {
        if (!team || !clean(team.id)) continue
        if (ctx.provider && ctx.provider.workspaceId && ctx.provider.workspaceId !== team.id) continue
        if (++count > 20) throw "IBM Bob returned more than 20 teams; select a workspace."
        const budget = request(ctx, base + "/admin/v1/teams/" + encodeURIComponent(team.id) + "/users/" + encodeURIComponent(instance.user_id),
          Object.assign({}, headers, { "x-instance-id": instance.instance_id, "x-team-id": team.id }))
        const used = number(budget.usage)
        const limit = number(budget.budget_limit) === null ? number(team.budget_limit) : number(budget.budget_limit)
        if (used === null) throw "IBM Bob did not return valid Bobcoin usage."
        const label = clean(instance.instance_name) || clean(instance.name) || instance.instance_id
        const teamName = clean(team.name) || team.id
        const reset = typeof instance.refresh_at === "number" ? instance.refresh_at * 1000 : instance.refresh_at
        if (limit !== null && limit > 0) {
          lines.push(ctx.line.progress({ label: label + " / " + teamName, used, limit,
            format: { kind: "count", suffix: "Bobcoins" }, resetsAt: ctx.util.toIso(reset),
            periodDurationMs: 30 * 24 * 60 * 60 * 1000 }))
        } else lines.push(ctx.line.text({ label: teamName, value: used + " Bobcoins used" }))
        if (clean(instance.plan_name) && !plans.includes(instance.plan_name)) plans.push(instance.plan_name)
      }
    }
    if (!lines.length) throw "No IBM Bob team subscription found."
    return { displayName: "IBM Bob", source: "api", plan: plans.join(", ") || undefined, lines }
  }
  globalThis.__openusage_plugin = { id: "ibmbob", probe }
})()
