(function () {
  var CLOUD_SERVICE = "exa.seat_management_pb.SeatManagementService"
  var DEFAULT_API_SERVER_URL = "https://server.codeium.com"
  var CLOUD_COMPAT_VERSION = "1.108.2"
  var APP_STATE_VARIANTS = [
    {id: "devin", source: "Devin app", appSupportRel: "Devin/User/globalStorage/state.vscdb"},
    {id: "devin-next", source: "Devin - Next app", appSupportRel: "Devin - Next/User/globalStorage/state.vscdb"},
    {id: "windsurf", source: "Devin app (legacy Windsurf)", appSupportRel: "Windsurf/User/globalStorage/state.vscdb"},
    {id: "windsurf-next", source: "Windsurf - Next app", appSupportRel: "Windsurf - Next/User/globalStorage/state.vscdb"},
  ]
  var LOGIN_HINT = "Run devin auth login or sign in to Devin and try again."
  var QUOTA_HINT = "Devin quota data unavailable. Try again later."
  var DAY_MS = 24 * 60 * 60 * 1000
  var WEEK_MS = 7 * DAY_MS

  function readFiniteNumber(value) {
    if (typeof value === "number") return Number.isFinite(value) ? value : null
    if (typeof value !== "string") return null
    var trimmed = value.trim()
    if (!trimmed) return null
    var parsed = Number(trimmed)
    return Number.isFinite(parsed) ? parsed : null
  }

  function clampPercent(value) {
    if (!Number.isFinite(value)) return 0
    if (value < 0) return 0
    if (value > 100) return 100
    return value
  }

  function readTomlString(text, key) {
    var lines = String(text || "").split(/\r?\n/)
    var prefix = new RegExp("^\\s*" + key + "\\s*=\\s*(.*)$")
    for (var i = 0; i < lines.length; i++) {
      var match = prefix.exec(lines[i])
      if (!match) continue
      var value = match[1].trim()
      if (!value) return null
      if (value[0] === '"' || value[0] === "'") {
        var quote = value[0]
        var out = ""
        for (var j = 1; j < value.length; j++) {
          var ch = value[j]
          if (ch === quote && value[j - 1] !== "\\") return out.trim() || null
          out += ch
        }
        return null
      }
      var commentIndex = value.indexOf("#")
      if (commentIndex >= 0) value = value.slice(0, commentIndex).trim()
      return value || null
    }
    return null
  }

  function cleanApiServerUrl(value) {
    if (typeof value !== "string") return null
    var trimmed = value.trim().replace(/\/+$/, "")
    if (!/^https:\/\//.test(trimmed)) return null
    return trimmed
  }

  function effectiveApiServerUrl(auth) {
    return (auth && auth.apiServerUrl) || DEFAULT_API_SERVER_URL
  }

  function hasOwn(obj, key) {
    return Boolean(obj && Object.prototype.hasOwnProperty.call(obj, key))
  }

  function readHost(value) {
    if (typeof value !== "string") return null
    var match = /^https?:\/\/([^/]+)/.exec(value.trim())
    return match ? match[1] : null
  }

  function valueOrMissing(value) {
    return value === null || value === undefined || value === "" ? "missing" : String(value)
  }

  function logQuotaDiagnostics(ctx, auth, userStatus) {
    var planStatus = (userStatus && userStatus.planStatus) || {}
    var planInfo = planStatus.planInfo || {}
    var devinInfo = planInfo.devinInfo || {}
    var apiServerHost = readHost(auth.apiServerUrl || DEFAULT_API_SERVER_URL)
    var webappHost = readHost(devinInfo.webappHost) || devinInfo.webappHost || null
    var devinApiHost = readHost(devinInfo.apiUrl)

    ctx.host.log.info(
      "Devin quota diagnostics" +
        " source=" + auth.source +
        " apiServerHost=" + valueOrMissing(apiServerHost) +
        " planName=" + valueOrMissing(planInfo.planName) +
        " teamsTier=" + valueOrMissing(userStatus && userStatus.teamsTier) +
        " planTeamsTier=" + valueOrMissing(planInfo.teamsTier) +
        " billingStrategy=" + valueOrMissing(planInfo.billingStrategy) +
        " isDevin=" + String(planInfo.isDevin === true) +
        " hideDailyQuota=" + String(planInfo.hideDailyQuota === true) +
        " hasDailyQuotaPercent=" + String(hasOwn(planStatus, "dailyQuotaRemainingPercent")) +
        " hasWeeklyQuotaPercent=" + String(hasOwn(planStatus, "weeklyQuotaRemainingPercent")) +
        " hasOverageBalance=" + String(hasOwn(planStatus, "overageBalanceMicros")) +
        " hasDailyReset=" + String(hasOwn(planStatus, "dailyQuotaResetAtUnix")) +
        " hasWeeklyReset=" + String(hasOwn(planStatus, "weeklyQuotaResetAtUnix")) +
        " hasTopUpStatus=" + String(hasOwn(planStatus, "topUpStatus")) +
        " availablePromptCredits=" + valueOrMissing(planStatus.availablePromptCredits) +
        " canUseCli=" + String(devinInfo.canUseCli === true) +
        " canUseCascade=" + String(devinInfo.canUseCascade === true) +
        " devinReviewEnabled=" + String(devinInfo.devinReviewEnabled === true) +
        " webappHost=" + valueOrMissing(webappHost) +
        " devinApiHost=" + valueOrMissing(devinApiHost)
    )
  }

  function settings(ctx) { return (ctx.provider && ctx.provider.settings) || {} }

  function resolveCredentialsPath(ctx) {
    var explicit = settings(ctx).credentialsPath
    if (explicit) return ctx.host.fs.exists(explicit) ? explicit : null
    // Carried CLI layout; credentialsPath supports other CLI versions without
    // guessing a different account's directory. Never search Unix paths on Windows.
    var paths = ctx.app.platform === "windows"
      ? [ctx.host.fs.localAppDataPath("devin/credentials.toml")]
      : ["~/.local/share/devin/credentials.toml", "~/.local/share/cognition/credentials.toml"]
    return ctx.host.fs.firstExisting(paths.filter(function(path) { return !!path })) || null
  }

  function resolveStateDbForVariant(ctx, variant) {
    var custom = settings(ctx).userDataDir
    var path = custom ? custom.replace(/[\\/]+$/, "") + "/User/globalStorage/state.vscdb"
      : ctx.host.fs.appSupportPath(variant.appSupportRel)
    return path && ctx.host.fs.exists(path) ? path : null
  }

  function loadCredentialsFile(ctx) {
    var credentialsPath = resolveCredentialsPath(ctx)
    if (!credentialsPath) return null
    try {
      var text = ctx.host.fs.readText(credentialsPath)
      var apiKey = readTomlString(text, "windsurf_api_key")
      if (!apiKey) {
        throw {code: "credential-malformed", message: "Selected Devin credentials do not contain windsurf_api_key; select the correct credentialsPath or sign in again."}
      }
      return {
        apiKey: apiKey,
        apiServerUrl: cleanApiServerUrl(readTomlString(text, "api_server_url")),
        source: "credentials.toml",
      }
    } catch (e) {
      if (e && e.code) throw e
      throw {code: "credential-unavailable", message: "Cannot read the selected Devin credential file."}
    }
  }

  function readAppAuth(ctx, variant) {
    var stateDb = resolveStateDbForVariant(ctx, variant)
    if (!stateDb) return null
    try {
      var rows = ctx.host.sqlite.query(
        stateDb,
        "SELECT value FROM ItemTable WHERE key = 'windsurfAuthStatus' LIMIT 1"
      )
      var parsed = ctx.util.tryParseJson(rows)
      if (!parsed || !parsed.length || !parsed[0].value) return null
      var auth = ctx.util.tryParseJson(parsed[0].value)
      if (!auth || !auth.apiKey) return null
      return {
        apiKey: auth.apiKey,
        apiServerUrl: null,
        source: variant.source,
      }
    } catch (e) {
      ctx.host.log.warn("failed to read " + variant.source + " auth: " + String(e))
      return null
    }
  }

  function callCloud(ctx, auth) {
    var apiServerUrl = effectiveApiServerUrl(auth)
    try {
      var resp = ctx.host.http.request({
        method: "POST",
        url: apiServerUrl + "/" + CLOUD_SERVICE + "/GetUserStatus",
        headers: {
          "Content-Type": "application/json",
          "Connect-Protocol-Version": "1",
        },
        bodyText: JSON.stringify({
          metadata: {
            apiKey: auth.apiKey,
            ideName: "devin",
            ideVersion: CLOUD_COMPAT_VERSION,
            extensionName: "devin",
            extensionVersion: CLOUD_COMPAT_VERSION,
            locale: "en",
          },
        }),
        timeoutMs: 15000,
      })
      if (resp.status < 200 || resp.status >= 300) {
        ctx.host.log.warn("cloud request returned status " + resp.status + " for " + auth.source)
        if (ctx.util && typeof ctx.util.isAuthStatus === "function" && ctx.util.isAuthStatus(resp.status)) {
          return { __openusageAuthError: true }
        }
        return null
      }
      return ctx.util.tryParseJson(resp.bodyText)
    } catch (e) {
      ctx.host.log.warn("cloud request failed for " + auth.source + ": " + String(e))
      return null
    }
  }

  function tryAuth(ctx, auth) {
    var data = callCloud(ctx, auth)
    if (data && data.__openusageAuthError) {
      return { authFailure: true }
    }
    if (!data || !data.userStatus) return {}

    try {
      logQuotaDiagnostics(ctx, auth, data.userStatus)
      return { output: buildOutput(ctx, data.userStatus) }
    } catch (e) {
      if (e === QUOTA_HINT) {
        ctx.host.log.warn("quota contract unavailable for " + auth.source)
        return {}
      }
      throw e
    }
  }

  function unixSecondsToIso(ctx, value) {
    var seconds = readFiniteNumber(value)
    if (seconds === null) return null
    return ctx.util.toIso(seconds * 1000)
  }

  function formatDollarsFromMicros(value) {
    var micros = readFiniteNumber(value)
    if (micros === null) return null
    if (!Number.isFinite(micros)) return null
    if (micros < 0) micros = 0
    return "$" + (micros / 1000000).toFixed(2)
  }

  function buildQuotaLine(ctx, label, remaining, resetsAt, periodDurationMs) {
    if (remaining === null) return null
    return buildUsedQuotaLine(ctx, label, 100 - remaining, resetsAt, periodDurationMs)
  }

  function buildUsedQuotaLine(ctx, label, used, resetsAt, periodDurationMs) {
    if (used === null) return null
    var line = {
      label: label,
      used: clampPercent(used),
      limit: 100,
      format: { kind: "percent" },
      periodDurationMs: periodDurationMs,
    }
    if (resetsAt) line.resetsAt = resetsAt
    return ctx.line.progress(line)
  }

  function buildOutput(ctx, userStatus) {
    var planStatus = (userStatus && userStatus.planStatus) || {}

    var planInfo = planStatus.planInfo || {}
    var planName = typeof planInfo.planName === "string" && planInfo.planName.trim()
      ? planInfo.planName.trim()
      : "Unknown"

    var hideDailyQuota = planInfo.hideDailyQuota === true
    var dailyRemaining = readFiniteNumber(planStatus.dailyQuotaRemainingPercent)
    var weeklyRemaining = readFiniteNumber(planStatus.weeklyQuotaRemainingPercent)
    var dailyReset = !hideDailyQuota ? unixSecondsToIso(ctx, planStatus.dailyQuotaResetAtUnix) : null
    var weeklyReset = unixSecondsToIso(ctx, planStatus.weeklyQuotaResetAtUnix)
    var extraUsageBalance = formatDollarsFromMicros(planStatus.overageBalanceMicros)

    var dailyLine = !hideDailyQuota
      ? buildQuotaLine(ctx, "Daily quota", dailyRemaining, dailyReset, DAY_MS)
      : null
    var weeklyLine = weeklyRemaining !== null
      ? buildQuotaLine(ctx, "Weekly quota", weeklyRemaining, weeklyReset, WEEK_MS)
      : hideDailyQuota
        ? buildUsedQuotaLine(ctx, "Weekly quota", dailyRemaining, weeklyReset, WEEK_MS)
        : null

    var lines = []
    if (dailyLine) lines.push(dailyLine)
    if (weeklyLine) lines.push(weeklyLine)
    if (extraUsageBalance) {
      lines.push(ctx.line.text({ label: "Extra usage balance", value: extraUsageBalance }))
    }

    if (!lines.length) throw QUOTA_HINT

    return {
      plan: planName,
      lines: lines,
    }
  }

  function authFingerprint(auth) {
    return auth.apiKey + "\n" + effectiveApiServerUrl(auth)
  }

  function firstPresent(values) {
    for (var i = 0; i < values.length; i++) if (values[i] !== null && values[i] !== undefined) return values[i]
    return null
  }

  function manualOrganization(raw) {
    if (typeof raw !== "string") throw {code: "missing-auth", message: "Set Devin workspace_id or DEVIN_ORGANIZATION to the intended organization."}
    var value = raw.trim().replace(/^\/+|\/+$/g, "")
    if (/^https:\/\//i.test(value)) {
      var url = /^https:\/\/(?:[a-z0-9-]+\.)*devin\.ai\/(org|organizations)\/([^/?#]+)(?:[/?#].*)?$/i.exec(value)
      if (!url) throw {code: "failed", message: "Use an app.devin.ai organization URL, organization ID, or slug."}
      value = url[1].toLowerCase() + "/" + url[2]
    }
    var parts = value.split("/")
    var id = parts.length === 1 ? parts[0] : parts[1]
    if (parts.length > 2 || (parts.length === 2 && !/^(org|organizations)$/.test(parts[0])) || !/^[a-z0-9][a-z0-9_.-]*$/i.test(id)) {
      throw {code: "failed", message: "Invalid Devin organization; use an organization ID, slug, or app.devin.ai organization URL."}
    }
    var internal = parts.length === 2 ? parts[0] === "organizations" : /^org[-_]/.test(id)
    return { id: id, internal: internal, path: (internal ? "organizations/" : "org/") + id }
  }

  function manualQuotaPercent(value) {
    var number = readFiniteNumber(value)
    if (number !== null) return number <= 1 ? number * 100 : number
    if (!value || typeof value !== "object") return null
    var usedPercent = firstPresent([value.used_percent, value.usedPercent, value.usage_percent, value.usagePercent, value.percent_used, value.percentUsed, value.percent])
    if (usedPercent !== null) return manualQuotaPercent(usedPercent)
    var remainingPercent = firstPresent([value.remaining_percent, value.remainingPercent, value.percent_remaining, value.percentRemaining])
    if (remainingPercent !== null) {
      var remaining = manualQuotaPercent(remainingPercent)
      return remaining === null ? null : 100 - remaining
    }
    var limit = readFiniteNumber(firstPresent([value.limit, value.quota, value.total, value.max]))
    var used = readFiniteNumber(firstPresent([value.used, value.usage, value.used_count, value.usedCount, value.consumed]))
    if (limit !== null && limit > 0 && used !== null) return used / limit * 100
    remaining = readFiniteNumber(firstPresent([value.remaining, value.left]))
    return limit !== null && limit > 0 && remaining !== null ? (limit - remaining) / limit * 100 : null
  }

  function manualWindow(ctx, data, cadence, depth) {
    if (!data || typeof data !== "object" || depth > 8) return null
    var current = readFiniteNumber(data[cadence + "_percentage"])
    if (current !== null) {
      var currentPercent = current < 1 ? current * 100 : current
      if (Number.isFinite(currentPercent)) return {used: currentPercent, reset: ctx.util.toIso(data[cadence + "_reset_at"])}
    }
    var singular = cadence === "daily" ? "day" : "week"
    var window = firstPresent([data[cadence], data[singular], data[cadence + "_quota"], data[cadence + "Quota"], data[cadence + "_usage"], data[cadence + "Usage"]])
    var percent = manualQuotaPercent(window)
    if (percent !== null && Number.isFinite(percent)) {
      return {used: percent, reset: ctx.util.toIso(window && typeof window === "object" ? firstPresent([window.resetsAt, window.reset_at, window.resetAt, window.reset_time]) : null)}
    }
    var keys = Object.keys(data)
    for (var i = 0; i < keys.length; i++) {
      var nested = manualWindow(ctx, data[keys[i]], cadence, depth + 1)
      if (nested) return nested
    }
    return null
  }

  function manualOutput(ctx, data) {
    var daily = manualWindow(ctx, data, "daily", 0)
    var weekly = manualWindow(ctx, data, "weekly", 0)
    var lines = []
    if (daily) lines.push(buildUsedQuotaLine(ctx, "Daily quota", daily.used, daily.reset, DAY_MS))
    if (weekly) lines.push(buildUsedQuotaLine(ctx, "Weekly quota", weekly.used, weekly.reset, WEEK_MS))
    if (!lines.length) throw {code: "no-data", message: "Devin web response did not contain measurable daily or weekly quotas."}
    var balance = readFiniteNumber(data.overage_balance)
    if (balance === null) {
      var cents = readFiniteNumber(data.overage_balance_cents)
      if (cents !== null) balance = cents / 100
    }
    if (balance !== null && balance >= 0) lines.push(ctx.line.text({label: "Extra usage balance", value: "$" + balance.toFixed(2)}))
    var plan = firstPresent([data.plan_name, data.planName, data.plan, data.tier, data.subscription_tier])
    return {source: "web", plan: typeof plan === "string" && plan.trim() ? plan.trim() : null, lines: lines}
  }

  function probeManual(ctx, opts) {
    if (opts.cookieSource === "off") throw {code: "missing-auth", message: "Devin web authentication is disabled in settings.cookieSource."}
    if (opts.credentialsPath || opts.ideVariant || opts.userDataDir || (opts.authSource && ["auto", "manual"].indexOf(opts.authSource) < 0)) {
      throw {code: "failed", message: "Select either Devin manual web authentication or a CLI/IDE credential source."}
    }
    var raw = firstPresent([ctx.host.env.get("DEVIN_BEARER_TOKEN"), ctx.host.env.get("DEVIN_AUTHORIZATION"), ctx.provider.cookieHeader, opts.bearerToken])
    var token = typeof raw === "string" ? raw.trim().replace(/^Authorization:\s*/i, "").replace(/^Bearer(?:\s+|$)/i, "").trim() : ""
    if (!token || /[^\x21-\x7e]/.test(token)) throw {code: "missing-auth", message: "Set a valid Devin bearer token in cookie_header or DEVIN_BEARER_TOKEN."}
    var org = manualOrganization(firstPresent([ctx.host.env.get("DEVIN_ORGANIZATION"), ctx.host.env.get("DEVIN_ORG"), ctx.provider.workspaceId, opts.organization]))
    var paths = org.internal ? [org.id, org.path] : [org.path, org.id]
    for (var i = 0; i < paths.length; i++) {
      var headers = {Authorization: "Bearer " + token, Accept: "application/json", "Accept-Language": "en-US,en;q=0.9", "User-Agent": "usagestat/" + ctx.app.version}
      if (org.internal) headers["x-cog-org-id"] = org.id
      var response = ctx.util.request({method: "GET", url: "https://app.devin.ai/api/" + paths[i] + "/billing/quota/usage", headers: headers, timeoutMs: 10000})
      if (response.status === 401 || response.status === 403) throw {code: "credential-denied", message: "Devin web token was rejected. Reauthenticate the selected organization."}
      // Alternate routes refer to the same organization; only missing routes
      // permit retry. A rejected token never selects a CLI or IDE account.
      if (response.status === 404 || response.status === 405) continue
      if (response.status !== 200) throw {code: "failed", message: "Devin quota request failed (HTTP " + response.status + ")."}
      var data = ctx.util.tryParseJson(response.bodyText)
      if (!data || typeof data !== "object") throw {code: "no-data", message: "Invalid Devin quota response."}
      return manualOutput(ctx, data)
    }
    throw {code: "no-data", message: "Devin quota endpoint was not found for the selected organization."}
  }

  function probe(ctx) {
    var opts = settings(ctx)
    var mode = ctx.sourceMode || "auto"
    var manual = mode === "web" || opts.authSource === "manual" || opts.cookieSource === "manual" ||
      (mode === "auto" && !opts.authSource && opts.cookieSource == null && (ctx.provider.cookieHeader != null || opts.bearerToken != null))
    if (manual) {
      if (mode === "local") throw {code: "failed", message: "Select web or auto mode to use Devin manual web authentication."}
      return probeManual(ctx, opts)
    }
    var source = opts.authSource || (opts.credentialsPath ? "cli" : opts.ideVariant || opts.userDataDir ? "ide" : "auto")
    if (["auto", "cli", "ide"].indexOf(source) < 0) throw {code: "failed", message: "Set Devin settings.authSource to auto, cli or ide."}
    if ((opts.credentialsPath && source !== "cli") || ((opts.ideVariant || opts.userDataDir) && source !== "ide")) {
      throw {code: "failed", message: "Select either a Devin CLI credential file or an IDE profile."}
    }
    var variants = APP_STATE_VARIANTS
    if (opts.ideVariant) {
      variants = variants.filter(function(variant) { return variant.id === opts.ideVariant })
      if (!variants.length) throw {code: "failed", message: "Unknown Devin ideVariant; use devin, devin-next, windsurf or windsurf-next."}
    }
    if (opts.userDataDir && !opts.ideVariant) throw {code: "failed", message: "Set ideVariant when selecting a Devin userDataDir."}
    var candidates = []
    if (source !== "ide") {
      var credentials = loadCredentialsFile(ctx)
      if (credentials) candidates.push(credentials)
    }
    if (source !== "cli") {
      for (var i = 0; i < variants.length; i++) {
        var auth = readAppAuth(ctx, variants[i])
        if (auth && !candidates.some(function(candidate) { return authFingerprint(candidate) === authFingerprint(auth) })) candidates.push(auth)
      }
    }
    if (candidates.length > 1) throw {code: "failed", message: "Multiple Devin accounts or installations found; select settings.authSource and credentialsPath or ideVariant."}
    if (!candidates.length) throw LOGIN_HINT
    // Once selected, a rejected token cannot silently select another account.
    var attempt = tryAuth(ctx, candidates[0])
    if (attempt.output) return attempt.output
    throw attempt.authFailure ? LOGIN_HINT : QUOTA_HINT
  }

  globalThis.__openusage_plugin = { id: "devin", probe: probe }
})()
