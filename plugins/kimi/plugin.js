(function () {
  const WEB_USAGE_URL = "https://www.kimi.com/apiv2/kimi.gateway.billing.v1.BillingService/GetUsages"
  const MEMBERSHIP_URL = "https://www.kimi.com/apiv2/kimi.gateway.membership.v2.MembershipService/"
  const WEEK_MS = 7 * 86400000

  function readNumber(value) {
    if (typeof value !== "number" && typeof value !== "string") return null
    if (typeof value === "string" && !value.trim()) return null
    const n = Number(value)
    return Number.isFinite(n) ? n : null
  }

  function titleCaseWords(value) {
    return String(value)
      .trim()
      .toLowerCase()
      .replace(/\b[a-z]/g, function (c) {
        return c.toUpperCase()
      })
  }

  function parsePlanLabel(data) {
    const level =
      data &&
      data.user &&
      data.user.membership &&
      typeof data.user.membership.level === "string"
        ? data.user.membership.level
        : null
    if (!level) return null

    if (level === "LEVEL_UNSPECIFIED") return null
    const names = { LEVEL_FREE: "Adagio", LEVEL_TRIAL: "Andante", LEVEL_BASIC: "Moderato", LEVEL_INTERMEDIATE: "Allegretto", LEVEL_ADVANCED: "Allegro" }
    if (data.version != null && data.version !== "GOODS_VERSION_V1") return level
    if (names[level]) return names[level]
    const cleaned = level.replace(/^LEVEL_/, "").replace(/_/g, " ")
    const label = titleCaseWords(cleaned)
    return label || null
  }

  function clean(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null
  }

  function env(ctx, name) {
    try { return clean(ctx.host.env.get(name)) } catch (_) { return null }
  }

  function readText(ctx, path) {
    try { return clean(ctx.host.fs.readText(path)) } catch (_) { return null }
  }

  function cliSession(ctx) {
    const override = env(ctx, "KIMI_CODE_HOME")
    const homes = override ? [override] : ["~/.kimi-code", "~/.kimi"]
    for (const home of homes) {
      const raw = readText(ctx, home + "/credentials/kimi-code.json")
      if (!raw) continue
      const credential = ctx.util.tryParseJson(raw)
      const expires = credential && readNumber(credential.expires_at)
      if (!credential || !clean(credential.access_token) || expires === null || expires <= Date.parse(ctx.nowIso) / 1000 + 60) continue
      let deviceId = readText(ctx, home + "/device_id")
      if (!deviceId) {
        // This is a client identity, not an authentication secret.
        deviceId = "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, function (c) {
          const n = Math.floor(Math.random() * 16)
          return (c === "x" ? n : (n & 3) | 8).toString(16)
        })
        try { ctx.host.fs.writeText(home + "/device_id", deviceId) } catch (_) {}
      }
      return { token: credential.access_token, headers: {
        "X-Msh-Platform": "kimi_code_cli", "X-Msh-Version": ctx.app.version,
        "X-Msh-Device-Model": ctx.app.platform,
        "X-Msh-Device-Id": deviceId.replace(/[^\x20-\x7e]/g, ""),
      } }
    }
    return null
  }

  function webToken(ctx) {
    const raw = clean(ctx.provider.cookieHeader) || env(ctx, "KIMI_AUTH_TOKEN")
    if (!raw) return null
    const cookie = raw.replace(/^Cookie:\s*/i, "")
    const match = /(?:^|;)\s*kimi-auth=([^;]+)/.exec(cookie)
    if (match) return clean(match[1])
    return cookie.includes("=") ? null : clean(cookie.replace(/^Bearer\s+/i, ""))
  }

  function webRequest(ctx, token, url, body, timeoutMs) {
    const payload = ctx.jwt.decodePayload(token) || {}
    const headers = {
      Authorization: "Bearer " + token, Cookie: "kimi-auth=" + token,
      "Content-Type": "application/json", Accept: "application/json",
      Origin: "https://www.kimi.com", Referer: "https://www.kimi.com/code/console",
      "connect-protocol-version": "1", "x-msh-platform": "web", "x-language": "en-US", "r-timezone": "UTC",
    }
    for (const pair of [["device_id", "x-msh-device-id"], ["ssid", "x-msh-session-id"], ["sub", "x-traffic-id"]]) {
      if (clean(payload[pair[0]])) headers[pair[1]] = payload[pair[0]].replace(/[^\x20-\x7e]/g, "")
    }
    return ctx.util.request({
      method: "POST", url, headers, bodyText: JSON.stringify(body), timeoutMs,
    })
  }

  function enrichMembership(ctx, result, token) {
    if (!token || ctx.provider.settings && ctx.provider.settings.cookieSource === "off") return result
    try {
      const response = webRequest(ctx, token, MEMBERSHIP_URL + "GetSubscriptionStats", {}, 2000)
      const data = response.status === 200 && ctx.util.tryParseJson(response.bodyText)
      if (!data) return result
      const pool = data.subscriptionBalance
      const ratio = pool && readNumber(pool.amountUsedRatio)
      if (Number.isFinite(ratio) && pool && (!pool.feature || pool.feature === "FEATURE_OMNI") && (!pool.type || pool.type === "SUBSCRIPTION")) {
        const reset = ctx.util.toIso(pool.expireTime)
        result.lines.push(ctx.line.progress({ label: "Monthly", used: Math.max(0, Math.min(100, ratio * 100)), limit: 100,
          format: { kind: "percent" }, resetsAt: reset, periodDurationMs: ctx.util.calendarMonthDuration(reset) }))
      }
      const weekly = data.ratelimitCode7d
      const weeklyRatio = weekly && readNumber(weekly.ratio)
      if (weekly && weekly.enabled !== false && Number.isFinite(weeklyRatio)) {
        const reset = ctx.util.toIso(weekly.resetTime)
        const used = Math.max(0, Math.min(100, weeklyRatio * 100))
        if (!result.lines.some((line) => line.label === "Weekly" && Math.abs(line.used - used) < 0.01 && (line.resetsAt || null) === reset)) {
          result.lines.push(ctx.line.progress({ label: "Code 7-day", used, limit: 100, format: { kind: "percent" }, resetsAt: reset, periodDurationMs: WEEK_MS }))
        }
      }
    } catch (_) { ctx.host.log.warn("Kimi membership enrichment unavailable") }
    return result
  }

  function parseWindowPeriodMs(window) {
    if (!window || typeof window !== "object") return null
    const duration = readNumber(window.duration)
    if (duration === null || duration <= 0) return null

    const unit = String(window.timeUnit || window.time_unit || "").toUpperCase()
    if (unit.indexOf("MINUTE") !== -1) return duration * 60 * 1000
    if (unit.indexOf("HOUR") !== -1) return duration * 60 * 60 * 1000
    if (unit.indexOf("DAY") !== -1) return duration * 24 * 60 * 60 * 1000
    if (unit.indexOf("SECOND") !== -1) return duration * 1000
    return null
  }

  function parseQuota(row, ctx) {
    if (!row || typeof row !== "object") return null

    const limit = readNumber(row.limit)
    if (limit === null || limit <= 0) return null

    let used = readNumber(row.used)
    if (used === null) {
      const remaining = readNumber(row.remaining)
      if (remaining !== null && remaining >= 0 && remaining <= limit) {
        used = limit - remaining
      }
    }
    if (used === null || used < 0) return null

    return {
      used,
      limit,
      resetsAt: ctx.util.toIso(row.resetTime || row.reset_at || row.resetAt || row.reset_time),
    }
  }

  function toPercentUsage(quota) {
    if (!quota || quota.limit <= 0) return null
    const usedPercent = (quota.used / quota.limit) * 100
    if (!Number.isFinite(usedPercent)) return null
    return {
      used: Math.round(Math.max(0, usedPercent) * 10) / 10,
      limit: 100,
      resetsAt: quota.resetsAt,
    }
  }

  function collectLimitCandidates(ctx, data) {
    const limits = Array.isArray(data && data.limits) ? data.limits : []
    const out = []

    for (let i = 0; i < limits.length; i += 1) {
      const item = limits[i]
      const detail = item && typeof item.detail === "object" ? item.detail : item
      const quota = parseQuota(detail, ctx)
      if (!quota) continue

      const periodMs = parseWindowPeriodMs(item && item.window)
      out.push({ quota, periodMs })
    }

    return out
  }

  function pickSessionCandidate(candidates) {
    if (!candidates.length) return null
    const sorted = candidates.slice().sort(function (a, b) {
      const aKnown = typeof a.periodMs === "number"
      const bKnown = typeof b.periodMs === "number"
      if (aKnown && bKnown) return a.periodMs - b.periodMs
      if (aKnown) return -1
      if (bKnown) return 1
      return 0
    })
    return sorted[0]
  }

  function pickLargestByPeriod(candidates) {
    if (!candidates.length) return null
    let best = candidates[0]
    for (let i = 1; i < candidates.length; i += 1) {
      const cur = candidates[i]
      const curMs = typeof cur.periodMs === "number" ? cur.periodMs : -1
      const bestMs = typeof best.periodMs === "number" ? best.periodMs : -1
      if (curMs > bestMs) best = cur
    }
    return best
  }

  function sameQuota(a, b) {
    if (!a || !b) return false
    return (
      a.quota.used === b.quota.used &&
      a.quota.limit === b.quota.limit &&
      (a.quota.resetsAt || null) === (b.quota.resetsAt || null)
    )
  }

  function probe(ctx) {
    const mode = ctx.sourceMode || "auto"
    if (mode === "local") throw "Kimi quotas require a live source. Select api, oauth, cli, or web."
    const key = clean(ctx.provider.apiKey) || env(ctx, "KIMI_CODE_API_KEY")
    const baseOverride = clean(ctx.provider.settings && ctx.provider.settings.baseUrl) || env(ctx, "KIMI_CODE_BASE_URL")
    const hasOverride = baseOverride || env(ctx, "KIMI_CODE_OAUTH_HOST") || env(ctx, "KIMI_OAUTH_HOST")
    const token = webToken(ctx)
    let source
    let resp
    if (mode === "api" && !key) throw "Set KIMI_CODE_API_KEY to a Kimi Code key, not an Open Platform key."
    const useKey = key && (mode === "auto" || mode === "api")
    const session = mode !== "web" && !useKey && !hasOverride ? cliSession(ctx) : null
    if (useKey || session) {
      const base = ctx.host.http.validateBaseUrl(baseOverride || "https://api.kimi.com", false)
      const url = base + (/\/coding\/v1$/.test(base) ? "/usages" : /\/coding$/.test(base) ? "/v1/usages" : "/coding/v1/usages")
      resp = ctx.util.request({ method: "GET", url, headers: Object.assign({}, session && session.headers,
        { Authorization: "Bearer " + (useKey ? key : session.token), Accept: "application/json", "User-Agent": "usagestat/" + ctx.app.version }), timeoutMs: 10000 })
      source = useKey ? "api" : "oauth"
    } else if (token && (mode === "web" || mode === "auto")) {
      resp = webRequest(ctx, token, WEB_USAGE_URL, { scope: ["FEATURE_CODING"] }, 10000)
      source = "web"
    } else {
      if (hasOverride) throw "Kimi endpoint overrides require an explicit API key; CLI credentials are never forwarded."
      throw "No fresh Kimi Code credentials. Run `kimi login`, set KIMI_CODE_API_KEY, or configure a web session."
    }

    if (ctx.util.isAuthStatus(resp.status)) {
      throw "Token expired. Run `kimi login` to authenticate."
    }
    if (resp.status < 200 || resp.status >= 300) {
      throw "Usage request failed (HTTP " + String(resp.status) + "). Try again later."
    }

    let data = ctx.util.tryParseJson(resp.bodyText)
    if (source === "web") {
      const coding = data && Array.isArray(data.usages) && data.usages.find((item) => item.scope === "FEATURE_CODING")
      data = coding ? { usage: coding.detail, limits: coding.limits } : null
    }
    if (!data || typeof data !== "object") {
      throw "Usage response invalid. Try again later."
    }

    const lines = []
    const candidates = collectLimitCandidates(ctx, data)
    const sessionCandidate = pickSessionCandidate(candidates)

    let weeklyCandidate = null
    const usageQuota = parseQuota(data.usage, ctx)
    if (usageQuota) {
      weeklyCandidate = { quota: usageQuota, periodMs: WEEK_MS }
    } else {
      const withoutSession = candidates.filter(function (candidate) {
        return candidate !== sessionCandidate
      })
      weeklyCandidate = pickLargestByPeriod(withoutSession)
    }

    if (sessionCandidate) {
      const sessionPercent = toPercentUsage(sessionCandidate.quota)
      if (sessionPercent) {
        lines.push(
          ctx.line.progress({
            label: "Session",
            used: sessionPercent.used,
            limit: sessionPercent.limit,
            format: { kind: "percent" },
            resetsAt: sessionPercent.resetsAt || undefined,
            periodDurationMs:
              typeof sessionCandidate.periodMs === "number"
                ? sessionCandidate.periodMs
                : undefined,
          })
        )
      }
    }

    if (weeklyCandidate && !sameQuota(weeklyCandidate, sessionCandidate)) {
      const weeklyPercent = toPercentUsage(weeklyCandidate.quota)
      if (weeklyPercent) {
        lines.push(
          ctx.line.progress({
            label: "Weekly",
            used: weeklyPercent.used,
            limit: weeklyPercent.limit,
            format: { kind: "percent" },
            resetsAt: weeklyPercent.resetsAt || undefined,
            periodDurationMs:
              typeof weeklyCandidate.periodMs === "number"
                ? weeklyCandidate.periodMs
                : undefined,
          })
        )
      }
    }

    if (lines.length === 0) {
      throw "Kimi usage response did not contain measurable quota counters."
    }

    return enrichMembership(ctx, {
      source,
      plan: parsePlanLabel(data),
      lines,
    }, token)
  }

  globalThis.__openusage_plugin = { id: "kimi", probe }
})()
