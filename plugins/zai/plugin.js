(function () {
  const HOUR_MS = 60 * 60 * 1000
  const WEEK_MS = 7 * 24 * HOUR_MS
  const MONTH_MS = 30 * 24 * HOUR_MS
  const INVALID = "Z.ai usage response invalid. Try again later."

  function clean(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null
  }

  function number(value) {
    if (typeof value !== "number" && typeof value !== "string") return null
    if (typeof value === "string" && !value.trim()) return null
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : null
  }

  function credentials(ctx) {
    const provider = ctx.provider || {}
    const settings = provider.settings || {}
    const env = (name) => clean(ctx.host.env.get(name))
    const cnKey = env("BIGMODEL_API_KEY") || env("ZHIPU_API_KEY")
    const key = clean(provider.apiKey) || env("ZAI_API_KEY") || env("Z_AI_API_KEY") || env("GLM_API_KEY") || cnKey
    if (!key) throw "No ZAI_API_KEY found. Set a provider API key or environment variable."
    const region = (clean(provider.region) || clean(settings.region) || env("Z_AI_REGION") || (key === cnKey ? "bigmodel-cn" : "global")).toLowerCase()
    const china = ["cn", "china", "bigmodel-cn"].includes(region)
    if (!china && region !== "global") throw "Unsupported Z.ai region. Use global or bigmodel-cn."
    if (key === cnKey && !china) throw "BigModel API keys must use the bigmodel-cn region."
    const headers = { Authorization: "Bearer " + key, Accept: "application/json" }
    const scope = clean(settings.usageScope) || env("Z_AI_USAGE_SCOPE") || "personal"
    if (!["personal", "team"].includes(scope)) throw "Unsupported Z.ai usage scope."
    if (scope === "team") {
      const organization = clean(settings.organization) || env("Z_AI_ORGANIZATION")
      const project = clean(settings.project) || clean(provider.workspaceId) || env("Z_AI_PROJECT")
      if (!organization || !project) throw "Z.ai team usage needs organization and project settings."
      headers["Bigmodel-Organization"] = organization
      headers["Bigmodel-Project"] = project
    }
    return { base: china ? "https://open.bigmodel.cn" : "https://api.z.ai", china, headers, scope }
  }

  function request(ctx, auth, url, timeoutMs) {
    const result = ctx.util.requestJson({ method: "GET", url, headers: auth.headers, timeoutMs })
    if (ctx.util.isAuthStatus(result.resp.status)) throw "Z.ai API key invalid. Check its region and credentials."
    if (result.resp.status < 200 || result.resp.status >= 300) throw "Z.ai API returned HTTP " + result.resp.status + "."
    const root = result.json
    if (!root || root.success === false || (root.code !== undefined && number(root.code) !== 200)) throw INVALID
    return root
  }

  function duration(entry) {
    const multipliers = { 1: 24 * HOUR_MS, 3: HOUR_MS, 5: 60000, 6: WEEK_MS }
    const unit = number(entry.unit)
    const count = number(entry.number)
    if ((entry.type || entry.name) === "TIME_LIMIT" && unit === 5 && count === 1) return MONTH_MS
    if (count > 0 && multipliers[unit]) return count * multipliers[unit]
    if (unit === 6) return WEEK_MS
    if (unit === 3 || unit === null) return 5 * HOUR_MS
    return null
  }

  function reset(ctx, entry, periodMs) {
    const raw = entry.nextResetTime
    let ms = number(raw)
    if (ms === null && typeof raw === "string") ms = Date.parse(raw)
    if (!Number.isFinite(ms) || ms < 1000000000000 || ms > 4102444800000) return undefined
    const now = Date.parse(ctx.nowIso) || Date.now()
    if (periodMs === 5 * HOUR_MS && ms > now + periodMs + 60000) return undefined
    return new Date(ms).toISOString()
  }

  function quotaLine(ctx, entry) {
    let used = number(entry.percentage)
    const cap = number(entry.usage)
    const current = number(entry.currentValue)
    const remaining = number(entry.remaining)
    if (cap > 0 && (current !== null || remaining !== null)) {
      used = Math.max(current === null ? 0 : current, remaining === null ? 0 : cap - remaining) / cap * 100
    }
    if (used === null) throw INVALID
    const periodMs = duration(entry)
    const label = periodMs === WEEK_MS ? "Weekly" : periodMs === 5 * HOUR_MS ? "Session" : "Quota"
    return ctx.line.progress({
      label, used: Math.max(0, Math.min(100, used)), limit: 100,
      format: { kind: "percent" }, periodDurationMs: periodMs || undefined,
      resetsAt: reset(ctx, entry, periodMs),
    })
  }

  function probe(ctx) {
    const auth = credentials(ctx)
    const root = request(ctx, auth, auth.base + "/api/monitor/usage/quota/limit" + (auth.scope === "team" ? "?type=2" : ""), 10000)
    const data = root.data || root
    const limits = Array.isArray(data) ? data : data.limits
    if (!Array.isArray(limits) || limits.some((item) => !item || typeof item !== "object")) throw INVALID
    const lines = []
    const quota = limits.filter((item) => ["TOKENS_LIMIT", "CREDIT_LIMIT"].includes(item.type || item.name))
      .sort((a, b) => (duration(a) || Infinity) - (duration(b) || Infinity))
    for (const item of quota) lines.push(quotaLine(ctx, item))
    const mcp = limits.find((item) => (item.type || item.name) === "TIME_LIMIT")
    if (mcp) {
      const used = number(mcp.currentValue)
      const limit = number(mcp.usage)
      if (used === null || limit === null || used < 0 || limit < 0) throw INVALID
      lines.push(ctx.line.progress({ label: "Web Searches", used, limit,
        format: { kind: "count", suffix: "searches" }, periodDurationMs: MONTH_MS,
        resetsAt: reset(ctx, mcp, MONTH_MS) }))
    }
    if (!lines.length) lines.push(ctx.line.badge({ label: "Status", text: "No usage data", color: "#a3a3a3" }))

    if (quota.some((item) => (item.type || item.name) === "CREDIT_LIMIT")) {
      const now = new Date(ctx.nowIso)
      const peak = now.getUTCDay() >= 1 && now.getUTCDay() <= 5 && now.getUTCHours() >= 6 && now.getUTCHours() < 10
      lines.push(ctx.line.text({ label: "Quota rate", value: peak ? "Peak" : "Off-peak",
        subtitle: "Peak: Mon-Fri 06:00-10:00 UTC" }))
    }

    let plan = clean(data.planName) || clean(data.plan) || clean(data.packageName)
    if (!plan) {
      try {
        const sub = request(ctx, auth, auth.base + "/api/biz/subscription/list", 1500)
        if (Array.isArray(sub.data) && sub.data.length) plan = clean(sub.data[0].productName)
      } catch (_) { ctx.host.log.warn("Z.ai plan details unavailable") }
    }
    if (auth.china) {
      try {
        const balance = request(ctx, auth, "https://www.bigmodel.cn/api/biz/account/query-customer-account-report", 3000)
        const account = balance.data || {}
        const available = number(account.availableBalance)
        const value = available !== null ? available : number(account.balance)
        if (value !== null) lines.push(ctx.line.text({ label: "Balance", value: "CNY " + value.toFixed(2) }))
      } catch (_) { ctx.host.log.warn("BigModel balance unavailable") }
    }
    return { displayName: "Z.ai", source: "api", plan: plan || undefined, lines }
  }

  globalThis.__openusage_plugin = { id: "zai", probe }
})()
