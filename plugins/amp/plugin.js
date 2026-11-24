(function () {
  var SECRETS_FILE = "~/.local/share/amp/secrets.json"
  var SECRETS_KEY = "apiKey@https://ampcode.com/"
  var API_URL = "https://ampcode.com/api/internal"

  function loadApiKey(ctx) {
    if (!ctx.host.fs.exists(SECRETS_FILE)) return null
    try {
      var text = ctx.host.fs.readText(SECRETS_FILE)
      var parsed = ctx.util.tryParseJson(text)
      if (parsed && parsed[SECRETS_KEY]) {
        ctx.host.log.info("api key loaded from secrets file")
        return parsed[SECRETS_KEY]
      }
    } catch (e) {
      ctx.host.log.warn("secrets file read failed: " + String(e))
    }
    return null
  }

  function fetchBalanceInfo(ctx, apiKey) {
    return ctx.util.requestJson({
      method: "POST",
      url: API_URL,
      headers: {
        "Authorization": "Bearer " + apiKey,
        "Content-Type": "application/json",
      },
      bodyText: JSON.stringify({ method: "userDisplayBalanceInfo", params: {} }),
      timeoutMs: 15000,
    })
  }

  function parseMoney(s) {
    return Number(s.replace(/,/g, ""))
  }

  function parseBalanceText(rawText) {
    if (!rawText || typeof rawText !== "string") return null
    var text = rawText.replace(/\u001b\[[0-9;]*[A-Za-z]/g, "").replace(/\*\*/g, "")

    var result = {
      remaining: null,
      total: null,
      hourlyRate: 0,
      bonusPct: null,
      bonusDays: null,
      credits: null,
      freeRemainingPct: null,
      workspaces: [],
      enterpriseWorkspaces: [],
    }

    var balanceMatch = text.match(/\$([0-9][0-9,]*(?:\.[0-9]+)?)\/\$([0-9][0-9,]*(?:\.[0-9]+)?) remaining/)
    if (balanceMatch) {
      var remaining = parseMoney(balanceMatch[1])
      var total = parseMoney(balanceMatch[2])
      if (Number.isFinite(remaining) && Number.isFinite(total)) {
        result.remaining = remaining
        result.total = total
      }
    }

    var percentMatch = text.match(/^\s*Amp Free:\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*%\s+remaining/im)
    if (percentMatch) {
      var pctRemaining = parseMoney(percentMatch[1])
      if (Number.isFinite(pctRemaining)) result.freeRemainingPct = Math.max(0, Math.min(100, pctRemaining))
    }

    var rateMatch = text.match(/replenishes \+\$([0-9][0-9,]*(?:\.[0-9]+)?)\/hour/)
    if (rateMatch) {
      var rate = parseMoney(rateMatch[1])
      if (Number.isFinite(rate)) result.hourlyRate = rate
    }

    var bonusMatch = text.match(/\+(\d+)% bonus for (\d+) more days?/)
    if (bonusMatch) {
      var pct = Number(bonusMatch[1])
      var days = Number(bonusMatch[2])
      if (Number.isFinite(pct) && Number.isFinite(days)) {
        result.bonusPct = pct
        result.bonusDays = days
      }
    }

    var creditsMatch = text.match(/Individual credits: \$([0-9][0-9,]*(?:\.[0-9]+)?) remaining/)
    if (creditsMatch) {
      var credits = parseMoney(creditsMatch[1])
      if (Number.isFinite(credits)) result.credits = credits
    }

    var workspacePattern = /^\s*Workspace\s+(.+?):\s*\$?([0-9][0-9,]*(?:\.[0-9]+)?)\s+remaining/gim
    var workspaceMatch
    while ((workspaceMatch = workspacePattern.exec(text))) {
      var workspaceRemaining = parseMoney(workspaceMatch[2])
      var workspaceName = workspaceMatch[1].trim()
      if (workspaceName && Number.isFinite(workspaceRemaining)) {
        result.workspaces.push({ name: workspaceName, remaining: workspaceRemaining })
      }
    }

    // Enterprise workspaces report who pays instead of a balance, so there is nothing to meter.
    var enterprisePattern = /^\s*Workspace\s+(.+?):\s*usage\s+paid\s+by\s+your\s+enterprise\b/gim
    var enterpriseMatch
    while ((enterpriseMatch = enterprisePattern.exec(text))) {
      var enterpriseName = enterpriseMatch[1].trim()
      if (enterpriseName) result.enterpriseWorkspaces.push(enterpriseName)
    }

    if (result.total === null && result.credits === null && result.freeRemainingPct === null &&
        !result.workspaces.length && !result.enterpriseWorkspaces.length) return null

    return result
  }

  function probe(ctx) {
    var apiKey = loadApiKey(ctx)
    if (!apiKey) {
      throw "Amp not installed. Install Amp Code to get started."
    }

    var result
    try {
      result = fetchBalanceInfo(ctx, apiKey)
    } catch (e) {
      ctx.host.log.error("balance info request failed: " + String(e))
      throw "Request failed. Check your connection."
    }

    var resp = result.resp
    var json = result.json

    if (resp.status === 401 || resp.status === 403) {
      throw "Session expired. Re-authenticate in Amp Code."
    }
    if (resp.status < 200 || resp.status >= 300) {
      var detail = json && json.error && json.error.message ? json.error.message : ""
      if (detail) {
        ctx.host.log.error("api returned " + resp.status + ": " + detail)
        throw detail
      }
      ctx.host.log.error("api returned: " + resp.status)
      throw "Request failed (HTTP " + resp.status + "). Try again later."
    }

    if (!json || !json.ok || !json.result || !json.result.displayText) {
      ctx.host.log.error("unexpected response structure")
      throw "Could not parse usage data."
    }

    var balance = parseBalanceText(json.result.displayText)
    if (!balance) {
      if (/Amp Free/.test(json.result.displayText)) {
        ctx.host.log.error("failed to parse display text: " + json.result.displayText)
        throw "Could not parse usage data."
      }
      ctx.host.log.warn("no balance data found, assuming credits-only: " + json.result.displayText)
      balance = {
        remaining: null,
        total: null,
        hourlyRate: 0,
        bonusPct: null,
        bonusDays: null,
        credits: 0,
        freeRemainingPct: null,
        workspaces: [],
        enterpriseWorkspaces: [],
      }
    }

    var lines = []
    var plan = "Free"

    if (balance.total !== null) {
      var used = Math.max(0, balance.total - balance.remaining)
      var total = balance.total

      var resetsAtMs = null
      if (used > 0 && balance.hourlyRate > 0) {
        var hoursToFull = used / balance.hourlyRate
        resetsAtMs = Date.now() + hoursToFull * 3600 * 1000
      }

      lines.push(ctx.line.progress({
        label: "Free",
        used: used,
        limit: total,
        format: { kind: "dollars" },
        resetsAt: ctx.util.toIso(resetsAtMs),
        periodDurationMs: 24 * 3600 * 1000,
      }))

      if (balance.bonusPct && balance.bonusDays) {
        lines.push(ctx.line.text({
          label: "Bonus",
          value: "+" + balance.bonusPct + "% for " + balance.bonusDays + "d",
        }))
      }
    }

    if (balance.total === null && balance.freeRemainingPct !== null) {
      lines.push(ctx.line.progress({
        label: "Free",
        used: 100 - balance.freeRemainingPct,
        limit: 100,
        format: { kind: "percent" },
        periodDurationMs: 24 * 3600 * 1000,
      }))
    }

    var hasFree = balance.total !== null || balance.freeRemainingPct !== null
    if (!hasFree && balance.enterpriseWorkspaces.length) plan = "Enterprise"
    else if (!hasFree && balance.credits !== null) plan = "Credits"

    if (balance.credits !== null && (balance.credits > 0 || !hasFree)) {
      lines.push(ctx.line.text({
        label: "Credits",
        value: "$" + balance.credits.toFixed(2),
      }))
    }

    for (var i = 0; i < balance.workspaces.length; i++) {
      lines.push(ctx.line.text({
        label: "Workspace " + balance.workspaces[i].name,
        value: "$" + balance.workspaces[i].remaining.toFixed(2),
      }))
    }

    for (var j = 0; j < balance.enterpriseWorkspaces.length; j++) {
      lines.push(ctx.line.text({ label: "Workspace " + balance.enterpriseWorkspaces[j], value: "Enterprise" }))
    }

    return { plan: plan, lines: lines }
  }

  globalThis.__openusage_plugin = { id: "amp", probe: probe }
})()
