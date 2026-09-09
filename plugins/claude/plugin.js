(function () {
  const DEFAULT_CLAUDE_HOME = "~/.claude"
  const CRED_FILE_NAME = ".credentials.json"
  const KEYCHAIN_SERVICE_PREFIX = "Claude Code"
  const PROD_BASE_API_URL = "https://api.anthropic.com"
  const PROD_REFRESH_URL = "https://platform.claude.com/v1/oauth/token"
  const ADMIN_COST_REPORT_URL = "https://api.anthropic.com/v1/organizations/cost_report"
  const ADMIN_MESSAGES_USAGE_URL = "https://api.anthropic.com/v1/organizations/usage_report/messages"
  const ANTHROPIC_VERSION = "2023-06-01"
  const ADMIN_DAILY_BUCKETS = 31
  const ADMIN_MAX_PAGES = 100
  const PROD_CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
  const NON_PROD_CLIENT_ID = "22422756-60c9-4084-8eb7-27705fd5cf9a"
  const SCOPES =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"
  const REFRESH_BUFFER_MS = 5 * 60 * 1000 // refresh 5 minutes before expiration
  const CLAUDE_WEB_BASE_URL = "https://claude.ai/api"

  // Rate-limit state persisted across probe() calls (module scope survives re-invocations).
  const MIN_USAGE_FETCH_INTERVAL_MS = 5 * 60 * 1000  // never poll more than once per 5 min
  const DEFAULT_RATE_LIMIT_BACKOFF_MS = 5 * 60 * 1000 // fallback when no Retry-After header
  const LIVE_USAGE_CACHE_FILE = "live-usage-cache.json"
  const LIVE_USAGE_CACHE_VERSION = 2
  const LIVE_USAGE_CACHE_MAX_AGE_MS = 24 * 60 * 60 * 1000
  let rateLimitedUntilMs = 0  // epoch ms; 0 = not rate-limited
  let lastUsageFetchMs = 0    // epoch ms of the most-recent OAuth API attempt
  let cachedUsageData = null  // last successful OAuth API response body (parsed JSON)
  let lastWebUsageFetchMs = 0   // epoch ms of the most-recent Web API attempt
  let cachedWebUsageData = null // last successful Web API response body (parsed JSON)
  let activeCacheIdentity = null

  function bindUsageCache(ctx, creds, sessionKey) {
    const identity = ctx.host.crypto.sha256(JSON.stringify([
      creds && creds.oauth && creds.oauth.accessToken || "", sessionKey || "",
      ctx.provider && ctx.provider.workspaceId || "",
    ]))
    if (activeCacheIdentity !== identity) _resetState()
    activeCacheIdentity = identity
    ctx.claudeCacheIdentity = identity
  }

  function utf8DecodeBytes(bytes) {
    // Prefer native TextDecoder when available (QuickJS may not expose it).
    if (typeof TextDecoder !== "undefined") {
      try {
        return new TextDecoder("utf-8", { fatal: false }).decode(new Uint8Array(bytes))
      } catch {}
    }

    // Minimal UTF-8 decoder (replacement char on invalid sequences).
    let out = ""
    for (let i = 0; i < bytes.length; ) {
      const b0 = bytes[i] & 0xff
      if (b0 < 0x80) {
        out += String.fromCharCode(b0)
        i += 1
        continue
      }

      // 2-byte
      if (b0 >= 0xc2 && b0 <= 0xdf) {
        if (i + 1 >= bytes.length) {
          out += "�"
          break
        }
        const b1 = bytes[i + 1] & 0xff
        if ((b1 & 0xc0) !== 0x80) {
          out += "�"
          i += 1
          continue
        }
        const cp = ((b0 & 0x1f) << 6) | (b1 & 0x3f)
        out += String.fromCharCode(cp)
        i += 2
        continue
      }

      // 3-byte
      if (b0 >= 0xe0 && b0 <= 0xef) {
        if (i + 2 >= bytes.length) {
          out += "�"
          break
        }
        const b1 = bytes[i + 1] & 0xff
        const b2 = bytes[i + 2] & 0xff
        const validCont = (b1 & 0xc0) === 0x80 && (b2 & 0xc0) === 0x80
        const notOverlong = !(b0 === 0xe0 && b1 < 0xa0)
        const notSurrogate = !(b0 === 0xed && b1 >= 0xa0)
        if (!validCont || !notOverlong || !notSurrogate) {
          out += "�"
          i += 1
          continue
        }
        const cp = ((b0 & 0x0f) << 12) | ((b1 & 0x3f) << 6) | (b2 & 0x3f)
        out += String.fromCharCode(cp)
        i += 3
        continue
      }

      // 4-byte
      if (b0 >= 0xf0 && b0 <= 0xf4) {
        if (i + 3 >= bytes.length) {
          out += "�"
          break
        }
        const b1 = bytes[i + 1] & 0xff
        const b2 = bytes[i + 2] & 0xff
        const b3 = bytes[i + 3] & 0xff
        const validCont = (b1 & 0xc0) === 0x80 && (b2 & 0xc0) === 0x80 && (b3 & 0xc0) === 0x80
        const notOverlong = !(b0 === 0xf0 && b1 < 0x90)
        const notTooHigh = !(b0 === 0xf4 && b1 > 0x8f)
        if (!validCont || !notOverlong || !notTooHigh) {
          out += "�"
          i += 1
          continue
        }
        const cp =
          ((b0 & 0x07) << 18) | ((b1 & 0x3f) << 12) | ((b2 & 0x3f) << 6) | (b3 & 0x3f)
        const n = cp - 0x10000
        out += String.fromCharCode(0xd800 + ((n >> 10) & 0x3ff), 0xdc00 + (n & 0x3ff))
        i += 4
        continue
      }

      out += "�"
      i += 1
    }
    return out
  }

  function tryParseCredentialJSON(ctx, text) {
    if (!text) return null
    const parsed = ctx.util.tryParseJson(text)
    if (parsed) return parsed

    // Some macOS keychain items are returned by `security ... -w` as hex-encoded UTF-8 bytes.
    // Example prefix: "7b0a" ( "{\\n" ).
    // Support both plain hex and "0x..." forms.
    let hex = String(text).trim()
    if (hex.startsWith("0x") || hex.startsWith("0X")) hex = hex.slice(2)
    if (!hex || hex.length % 2 !== 0) return null
    if (!/^[0-9a-fA-F]+$/.test(hex)) return null
    try {
      const bytes = []
      for (let i = 0; i < hex.length; i += 2) {
        bytes.push(parseInt(hex.slice(i, i + 2), 16))
      }
      const decoded = utf8DecodeBytes(bytes)
      const decodedParsed = ctx.util.tryParseJson(decoded)
      if (decodedParsed) return decodedParsed
    } catch {}

    return null
  }

  function readEnvText(ctx, name) {
    try {
      const value = ctx.host.env.get(name)
      if (value === null || value === undefined) return null
      const text = name === "CLAUDE_CONFIG_DIR" ? String(value) : String(value).trim()
      return text || null
    } catch {
      return null
    }
  }

  function readEnvFlag(ctx, name) {
    const value = readEnvText(ctx, name)
    if (!value) return false
    const lower = value.toLowerCase()
    return lower !== "0" && lower !== "false" && lower !== "no" && lower !== "off"
  }

  function cleanText(value) {
    if (value === null || value === undefined) return null
    let text = String(value).trim()
    if (!text) return null
    if ((text.startsWith('"') && text.endsWith('"')) || (text.startsWith("'") && text.endsWith("'"))) {
      text = text.slice(1, -1).trim()
    }
    return text || null
  }

  function readClaudeAdminApiKey(ctx) {
    const configured = cleanText(ctx.provider && ctx.provider.apiKey)
    if (configured) return configured

    const envNames = ["ANTHROPIC_ADMIN_KEY", "ANTHROPIC_ADMIN_API_KEY"]
    for (let i = 0; i < envNames.length; i++) {
      const value = cleanText(readEnvText(ctx, envNames[i]))
      if (value) return value
    }
    return null
  }

  function liveUsageCachePath(ctx) {
    if (!ctx.app || typeof ctx.app.pluginDataDir !== "string" || !ctx.app.pluginDataDir.trim()) {
      return null
    }
    return ctx.app.pluginDataDir + "/" + LIVE_USAGE_CACHE_FILE
  }

  function readLiveUsageCache(ctx) {
    const path = liveUsageCachePath(ctx)
    if (!path || !ctx.host.fs.exists(path)) return null
    try {
      const parsed = ctx.util.tryParseJson(ctx.host.fs.readText(path))
      if (!parsed || typeof parsed !== "object" || parsed.accountHash !== ctx.claudeCacheIdentity) return null
      return parsed
    } catch {
      return null
    }
  }

  function writeLiveUsageCache(ctx, updates) {
    const path = liveUsageCachePath(ctx)
    if (!path) return
    try {
      const cache = readLiveUsageCache(ctx) || {}
      for (const key in updates) {
        cache[key] = updates[key]
      }
      cache.version = LIVE_USAGE_CACHE_VERSION
      cache.accountHash = ctx.claudeCacheIdentity
      cache.updatedAtMs = Date.now()
      ctx.host.fs.writeText(path, JSON.stringify(cache, null, 2))
    } catch (e) {
      ctx.host.log.warn("Claude live usage cache write failed: " + String(e))
    }
  }

  function cachedLiveUsageData(cache, nowMs) {
    if (!cache || !cache.usageData || typeof cache.usageData !== "object") return null
    const fetchedAtMs = Number(cache.usageFetchedAtMs)
    if (!Number.isFinite(fetchedAtMs) || fetchedAtMs <= 0) return null
    if (nowMs - fetchedAtMs > LIVE_USAGE_CACHE_MAX_AGE_MS) return null
    return cache.usageData
  }

  function getClaudeHomePath(ctx) {
    return readEnvText(ctx, "CLAUDE_CONFIG_DIR") || DEFAULT_CLAUDE_HOME
  }

  function getClaudeHomeOverride(ctx) {
    return readEnvText(ctx, "CLAUDE_CONFIG_DIR")
  }

  function getClaudeCredentialsPath(ctx) {
    return getClaudeHomePath(ctx) + "/" + CRED_FILE_NAME
  }

  function getOauthConfig(ctx) {
    let baseApiUrl = PROD_BASE_API_URL
    let refreshUrl = PROD_REFRESH_URL
    let clientId = PROD_CLIENT_ID
    let oauthFileSuffix = ""

    const isAntUser = readEnvText(ctx, "USER_TYPE") === "ant"
    if (isAntUser && readEnvFlag(ctx, "USE_LOCAL_OAUTH")) {
      const localApiBase = readEnvText(ctx, "CLAUDE_LOCAL_OAUTH_API_BASE")
      baseApiUrl = (localApiBase || "http://localhost:8000").replace(/\/+$/, "")
      refreshUrl = baseApiUrl + "/v1/oauth/token"
      clientId = NON_PROD_CLIENT_ID
      oauthFileSuffix = "-local-oauth"
    } else if (isAntUser && readEnvFlag(ctx, "USE_STAGING_OAUTH")) {
      baseApiUrl = "https://api-staging.anthropic.com"
      refreshUrl = "https://platform.staging.ant.dev/v1/oauth/token"
      clientId = NON_PROD_CLIENT_ID
      oauthFileSuffix = "-staging-oauth"
    }

    const customOauthBase = readEnvText(ctx, "CLAUDE_CODE_CUSTOM_OAUTH_URL")
    if (customOauthBase) {
      const base = customOauthBase.replace(/\/+$/, "")
      baseApiUrl = base
      refreshUrl = base + "/v1/oauth/token"
      oauthFileSuffix = "-custom-oauth"
    }

    const clientIdOverride = readEnvText(ctx, "CLAUDE_CODE_OAUTH_CLIENT_ID")
    if (clientIdOverride) {
      clientId = clientIdOverride
    }

    return {
      baseApiUrl: baseApiUrl,
      usageUrl: baseApiUrl + "/api/oauth/usage",
      refreshUrl: refreshUrl,
      clientId: clientId,
      oauthFileSuffix: oauthFileSuffix,
    }
  }

  function buildClaudeBaseKeychainService(ctx) {
    return KEYCHAIN_SERVICE_PREFIX + getOauthConfig(ctx).oauthFileSuffix + "-credentials"
  }

  function computeKeychainHashSuffix(ctx) {
    // Mirrors upstream Claude Code (decompiled from the binary):
    //   const suffix = !process.env.CLAUDE_CONFIG_DIR
    //     ? ""
    //     : "-" + sha256(CLAUDE_CONFIG_DIR.normalize("NFC")).slice(0, 8)
    // The hash is ONLY appended when CLAUDE_CONFIG_DIR is explicitly set;
    // when unset, upstream uses the legacy unhashed service name.
    const explicitConfigDir = readEnvText(ctx, "CLAUDE_CONFIG_DIR")
    if (!explicitConfigDir) return null
    const sha256Hex = ctx.host && ctx.host.crypto && ctx.host.crypto.sha256Hex
    if (typeof sha256Hex !== "function") return null
    // Match upstream's `.normalize("NFC")` exactly.
    const normalized =
      typeof explicitConfigDir.normalize === "function"
        ? explicitConfigDir.normalize("NFC")
        : explicitConfigDir
    const digest = sha256Hex(normalized)
    if (typeof digest !== "string" || digest.length < 8) return null
    return digest.slice(0, 8)
  }

  function getClaudeKeychainServiceCandidates(ctx) {
    const base = buildClaudeBaseKeychainService(ctx)
    if (!readEnvText(ctx, "CLAUDE_CONFIG_DIR")) return [base]
    const hash = computeKeychainHashSuffix(ctx)
    if (!hash) throw {code: "failed", message: "The host cannot resolve this Claude profile's credential service."}
    return [base + "-" + hash]
  }

  function readKeychainCredentialText(ctx, service) {
    const keychain = ctx.host.keychain
    if (!keychain) return null
    const currentUser = typeof keychain.readGenericPasswordForCurrentUser === "function"
    const read = currentUser ? keychain.readGenericPasswordForCurrentUser : keychain.readGenericPassword
    if (typeof read !== "function") return null
    try {
      const value = read.call(keychain, service)
      return value ? {value, source: currentUser ? "keychain-current-user" : "keychain-legacy"} : null
    } catch (e) {
      // Missing auth can fall back to this profile's file. Denied/locked stores
      // must remain distinguishable, and never broaden the account lookup.
      if (/credential-(denied|unavailable|account-mismatch|malformed):/.test(String(e))) throw e
      return null
    }
  }

  function loadFileCredentials(ctx) {
    const credFile = getClaudeCredentialsPath(ctx)
    if (ctx.host.fs.exists(credFile)) {
      try {
        const text = ctx.host.fs.readText(credFile)
        const parsed = tryParseCredentialJSON(ctx, text)
        if (parsed) {
          const oauth = parsed.claudeAiOauth
          if (oauth && oauth.accessToken) {
            ctx.host.log.info("credentials loaded from file")
            return { oauth, source: "file", fullData: parsed }
          }
        }
        ctx.host.log.warn("credentials file exists but no valid oauth data")
      } catch (e) {
        ctx.host.log.warn("credentials file read failed: " + String(e))
      }
    }

    return null
  }

  function loadKeychainCredentials(ctx) {
    // Upstream uses Keychain on macOS and profile files on Linux/Windows.
    if (ctx.app.platform !== "macos") return null
    // An explicit profile reads only its own hashed service.
    for (const service of getClaudeKeychainServiceCandidates(ctx)) {
      const keychainResult = readKeychainCredentialText(ctx, service)
      if (keychainResult && keychainResult.value) {
        const parsed = tryParseCredentialJSON(ctx, keychainResult.value)
        if (parsed) {
          const oauth = parsed.claudeAiOauth
          if (oauth && oauth.accessToken) {
            ctx.host.log.info("credentials loaded from keychain (service=" + service + ")")
            return { oauth, source: keychainResult.source, serviceName: service, fullData: parsed }
          }
        }
        ctx.host.log.warn("keychain has data for " + service + " but no valid oauth")
        // Continue: a stale legacy entry shouldn't shadow a valid hashed one.
      }
    }

    return null
  }

  function loadStoredCredentials(ctx, suppressMissingWarn) {
    // Recent Claude Code versions keep the current session in Keychain and can
    // leave a stale credentials file behind, so Keychain wins when valid.
    let keychainError = null
    let keychainCredentials = null
    try { keychainCredentials = loadKeychainCredentials(ctx) } catch (e) { keychainError = e }
    if (keychainCredentials) return keychainCredentials

    const fileCredentials = loadFileCredentials(ctx)
    if (fileCredentials) return fileCredentials

    if (keychainError) throw keychainError

    if (!suppressMissingWarn) {
      ctx.host.log.warn("no credentials found")
    }
    return null
  }

  function loadCredentials(ctx) {
    const injected = readInjectedCredential(ctx)
    if (injected) return injected

    const envAccessToken = readEnvText(ctx, "CLAUDE_CODE_OAUTH_TOKEN")
    const stored = loadStoredCredentials(ctx, !!envAccessToken)
    if (!envAccessToken) {
      return stored
    }

    const oauth = stored && stored.oauth ? Object.assign({}, stored.oauth) : {}
    oauth.accessToken = envAccessToken
    return {
      oauth: oauth,
      source: stored ? stored.source : null,
      serviceName: stored ? stored.serviceName : null,
      fullData: stored ? stored.fullData : null,
      inferenceOnly: true,
    }
  }

  function readInjectedCredential(ctx) {
    try {
      if (!ctx.host.credentials || typeof ctx.host.credentials.get !== "function") return null
      const raw = ctx.host.credentials.get()
      if (!raw) return null
      const credential = ctx.util.tryParseJson(String(raw))
      if (!credential) return null
      const accessToken = String(credential.accessToken || credential.sessionKey || "").trim()
      const refreshToken = String(credential.refreshToken || "").trim()
      if (!accessToken && !refreshToken) return null
      return {
        oauth: {
          accessToken,
          refreshToken,
          expiresAt: typeof credential.expiresAt === "number" ? credential.expiresAt : null,
          scopes: ["user:profile"],
        },
        source: "provider-account",
        serviceName: null,
        fullData: { claudeAiOauth: {} },
      }
    } catch (e) {
      ctx.host.log.warn("provider account credential read failed: " + String(e))
      return null
    }
  }

  function hasProfileScope(creds) {
    if (!creds || creds.inferenceOnly) {
      return false
    }
    const scopes = creds.oauth && creds.oauth.scopes
    if (Array.isArray(scopes) && scopes.length > 0) {
      return scopes.indexOf("user:profile") !== -1
    }
    return true
  }

  function saveCredentials(ctx, source, serviceName, fullData) {
    if (source === "provider-account") {
      try {
        if (ctx.host.credentials && typeof ctx.host.credentials.update === "function") {
          const oauth = fullData && fullData.claudeAiOauth ? fullData.claudeAiOauth : {}
          ctx.host.credentials.update(JSON.stringify({
            accessToken: oauth.accessToken || null,
            refreshToken: oauth.refreshToken || null,
            expiresAt: oauth.expiresAt || null,
          }))
        }
      } catch (e) {
        ctx.host.log.error("Failed to update provider account credentials: " + String(e))
      }
      return
    }
    // MUST use minified JSON - macOS `security -w` hex-encodes values with newlines,
    // which Claude Code can't read back, causing it to invalidate the session.
    const text = JSON.stringify(fullData)
    if (source === "file") {
      try {
        ctx.host.fs.writeText(getClaudeCredentialsPath(ctx), text)
      } catch (e) {
        ctx.host.log.error("Failed to write Claude credentials file: " + String(e))
      }
      return
    }
    if (!serviceName) {
      ctx.host.log.error("Refusing keychain write: missing service name (source=" + source + ")")
      return
    }
    if (source === "keychain-current-user") {
      try {
        if (typeof ctx.host.keychain.writeGenericPasswordForCurrentUser === "function") {
          ctx.host.keychain.writeGenericPasswordForCurrentUser(serviceName, text)
        } else {
          ctx.host.keychain.writeGenericPassword(serviceName, text)
        }
      } catch (e) {
        ctx.host.log.error("Failed to write Claude credentials keychain: " + String(e))
      }
    } else if (source === "keychain-legacy" || source === "keychain") {
      try {
        ctx.host.keychain.writeGenericPassword(serviceName, text)
      } catch (e) {
        ctx.host.log.error("Failed to write Claude credentials keychain: " + String(e))
      }
    }
  }

  function needsRefresh(ctx, oauth, nowMs) {
    return ctx.util.needsRefreshByExpiry({
      nowMs,
      expiresAtMs: oauth.expiresAt,
      bufferMs: REFRESH_BUFFER_MS,
    })
  }

  function refreshToken(ctx, creds) {
    const { oauth, source, fullData } = creds
    if (!oauth.refreshToken) {
      ctx.host.log.warn("refresh skipped: no refresh token")
      return null
    }

    const oauthConfig = getOauthConfig(ctx)
    ctx.host.log.info("attempting token refresh")
    try {
      const resp = ctx.util.request({
        method: "POST",
        url: oauthConfig.refreshUrl,
        headers: { "Content-Type": "application/json" },
        bodyText: JSON.stringify({
          grant_type: "refresh_token",
          refresh_token: oauth.refreshToken,
          client_id: oauthConfig.clientId,
          scope: SCOPES,
        }),
        timeoutMs: 15000,
      })

      if (resp.status === 400 || resp.status === 401) {
        let errorCode = null
        const body = ctx.util.tryParseJson(resp.bodyText)
        if (body) errorCode = body.error || body.error_description
        ctx.host.log.error("refresh failed: status=" + resp.status + " error=" + String(errorCode))
        if (errorCode === "invalid_grant") {
          throw "Session expired. Run `claude` to log in again."
        }
        throw "Token expired. Run `claude` to log in again."
      }
      if (resp.status < 200 || resp.status >= 300) {
        ctx.host.log.warn("refresh returned unexpected status: " + resp.status)
        return null
      }

      const body = ctx.util.tryParseJson(resp.bodyText)
      if (!body) {
        ctx.host.log.warn("refresh response not valid JSON")
        return null
      }
      const newAccessToken = body.access_token
      if (!newAccessToken) {
        ctx.host.log.warn("refresh response missing access_token")
        return null
      }

      // Update oauth credentials
      oauth.accessToken = newAccessToken
      if (body.refresh_token) oauth.refreshToken = body.refresh_token
      if (typeof body.expires_in === "number") {
        oauth.expiresAt = Date.now() + body.expires_in * 1000
      }

      // Persist updated credentials back to the same source we read from.
      fullData.claudeAiOauth = oauth
      saveCredentials(ctx, source, creds.serviceName, fullData)
      bindUsageCache(ctx, creds, getSessionKey(ctx))

      ctx.host.log.info("refresh succeeded, new token expires in " + (body.expires_in || "unknown") + "s")
      return newAccessToken
    } catch (e) {
      if (typeof e === "string") throw e
      ctx.host.log.error("refresh exception: " + String(e))
      return null
    }
  }

  function fetchUsage(ctx, accessToken) {
    const oauthConfig = getOauthConfig(ctx)
    return ctx.util.request({
      method: "GET",
      url: oauthConfig.usageUrl,
      headers: {
        Authorization: "Bearer " + accessToken.trim(),
        Accept: "application/json",
        "Content-Type": "application/json",
        "anthropic-beta": "oauth-2025-04-20",
        "User-Agent": "claude-code/2.1.69",
      },
      timeoutMs: 10000,
    })
  }

  function parseRetryAfterSeconds(headers) {
    if (!headers) return null
    const raw = headers["retry-after"] ?? headers["Retry-After"]
    if (raw === undefined || raw === null) return null
    const str = String(raw).trim()
    if (!str) return null
    // Retry-After can be a delay-seconds or HTTP-date (RFC 7231).
    // 0 means "retry immediately" — return 0 as a valid value.
    const seconds = /^\d+$/.test(str) ? Number(str) : NaN
    if (Number.isSafeInteger(seconds)) return seconds
    const dateMs = Date.parse(str)
    if (Number.isFinite(dateMs)) {
      const delay = Math.ceil((dateMs - Date.now()) / 1000)
      return delay > 0 ? delay : 0
    }
    return null
  }

  function fmtRateLimitMinutes(seconds) {
    if (seconds <= 0) return "now"
    const mins = Math.ceil(seconds / 60)
    return mins + "m"
  }

  function queryTokenUsage(ctx, homePath) {
    if (!ctx.host.ccusage || typeof ctx.host.ccusage.query !== "function") {
      return { status: "unavailable", data: null }
    }

    const since = new Date()
    // Inclusive range: today + previous 30 days = 31 calendar days.
    since.setDate(since.getDate() - 30)
    const y = since.getFullYear()
    const m = since.getMonth() + 1
    const d = since.getDate()
    const sinceStr = "" + y + (m < 10 ? "0" : "") + m + (d < 10 ? "0" : "") + d

    const queryOpts = { since: sinceStr }
    if (homePath) {
      queryOpts.homePath = homePath
    }

    const result = ctx.host.ccusage.query(queryOpts)
    if (!result || typeof result !== "object" || typeof result.status !== "string") {
      return { status: "runner_failed", data: null }
    }
    if (result.status !== "ok") {
      return { status: result.status, data: null }
    }
    if (!result.data || !Array.isArray(result.data.daily)) {
      return { status: "runner_failed", data: null }
    }
    return { status: "ok", data: result.data }
  }

  function adminIsoString(ms) {
    return new Date(ms).toISOString().replace(".000Z", "Z")
  }

  function adminRange(nowMs) {
    const now = new Date(nowMs)
    const todayStartMs = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate())
    const startMs = todayStartMs - (ADMIN_DAILY_BUCKETS - 1) * 24 * 60 * 60 * 1000
    const endMs = todayStartMs + 24 * 60 * 60 * 1000
    return { startingAt: adminIsoString(startMs), endingAt: adminIsoString(endMs) }
  }

  function makeAdminUrl(baseUrl, params) {
    const query = []
    const keys = Object.keys(params)
    for (let i = 0; i < keys.length; i++) {
      const key = keys[i]
      const value = params[key]
      if (value === null || value === undefined || value === "") continue
      query.push(encodeURIComponent(key) + "=" + encodeURIComponent(String(value)))
    }
    return baseUrl + (query.length ? "?" + query.join("&") : "")
  }

  function makeAdminError(message, status, endpoint) {
    return {
      message,
      status,
      endpoint,
      authRejected: status === 401 || status === 403,
    }
  }

  function requestClaudeAdminJson(ctx, url, apiKey, endpoint) {
    let resp
    try {
      resp = ctx.util.request({
        method: "GET",
        url,
        headers: {
          "x-api-key": apiKey,
          "anthropic-version": ANTHROPIC_VERSION,
          Accept: "application/json",
          "User-Agent": "UsageStat/1.0",
        },
        timeoutMs: 20000,
      })
    } catch (e) {
      throw makeAdminError("Claude Admin API " + endpoint + " request failed: " + String(e), 0, endpoint)
    }

    if (ctx.util.isAuthStatus(resp.status)) {
      throw makeAdminError("Claude Admin API key invalid or missing required permissions.", resp.status, endpoint)
    }
    if (resp.status < 200 || resp.status >= 300) {
      throw makeAdminError("Claude Admin API " + endpoint + " failed (HTTP " + resp.status + ").", resp.status, endpoint)
    }

    const json = ctx.util.tryParseJson(resp.bodyText)
    if (!json) throw makeAdminError("Claude Admin API " + endpoint + " response invalid.", resp.status, endpoint)
    return json
  }

  function fetchClaudeAdminReport(ctx, apiKey, url, endpoint, groupBy) {
    const range = adminRange(Date.now())
    const buckets = []
    let nextPage = null
    const seenPages = {}
    let pageCount = 0

    do {
      pageCount += 1
      if (pageCount > ADMIN_MAX_PAGES) {
        throw makeAdminError("Claude Admin API " + endpoint + " pagination exceeded " + ADMIN_MAX_PAGES + " pages.", 200, endpoint)
      }

      const params = {
        starting_at: range.startingAt,
        ending_at: range.endingAt,
        bucket_width: "1d",
        limit: ADMIN_DAILY_BUCKETS,
        "group_by[]": groupBy,
      }
      if (nextPage) params.page = nextPage

      const json = requestClaudeAdminJson(ctx, makeAdminUrl(url, params), apiKey, endpoint)
      if (Array.isArray(json.data)) {
        for (let i = 0; i < json.data.length; i++) buckets.push(json.data[i])
      }

      if (!json.has_more) {
        nextPage = null
        continue
      }
      nextPage = cleanText(json.next_page)
      if (!nextPage) {
        throw makeAdminError("Claude Admin API " + endpoint + " pagination cursor missing.", 200, endpoint)
      }
      if (seenPages[nextPage]) {
        throw makeAdminError("Claude Admin API " + endpoint + " pagination cursor repeated.", 200, endpoint)
      }
      seenPages[nextPage] = true
    } while (nextPage)

    return buckets
  }

  function numberValue(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value
    if (typeof value === "string" && value.trim()) {
      const parsed = Number(value.trim().replace(/,/g, ""))
      if (Number.isFinite(parsed)) return parsed
    }
    return null
  }

  function intValue(value) {
    const n = numberValue(value)
    return n === null ? 0 : Math.max(0, Math.floor(n))
  }

  function displayName(raw, fallback) {
    return cleanText(raw) || fallback
  }

  function dayKeyFromAdminDate(rawDate) {
    const ms = Date.parse(rawDate)
    if (!Number.isFinite(ms)) return null
    return new Date(ms).toISOString().slice(0, 10)
  }

  function adminDailyBucket(map, startingAt, endingAt) {
    const dayKey = dayKeyFromAdminDate(startingAt)
    if (!dayKey) return null
    if (!map[dayKey]) {
      map[dayKey] = {
        date: dayKey,
        startingAt,
        endingAt,
        costUSD: 0,
        inputTokens: 0,
        cacheCreationTokens: 0,
        cacheReadTokens: 0,
        outputTokens: 0,
        totalTokens: 0,
        costItems: {},
        models: {},
      }
    }
    return map[dayKey]
  }

  function addAdminModel(day, name, input, cacheCreation, cacheRead, output, total) {
    if (!day.models[name]) {
      day.models[name] = {
        modelName: name,
        inputTokens: 0,
        cacheCreationTokens: 0,
        cacheReadTokens: 0,
        outputTokens: 0,
        totalTokens: 0,
      }
    }
    const model = day.models[name]
    model.inputTokens += input
    model.cacheCreationTokens += cacheCreation
    model.cacheReadTokens += cacheRead
    model.outputTokens += output
    model.totalTokens += total
  }

  function cacheCreationTokenCount(cacheCreation) {
    if (!cacheCreation || typeof cacheCreation !== "object") return 0
    const total = intValue(cacheCreation.total_input_tokens)
    if (total > 0) return total
    return intValue(cacheCreation.ephemeral_1h_input_tokens) +
      intValue(cacheCreation.ephemeral_5m_input_tokens)
  }

  function makeClaudeAdminDaily(costBuckets, messageBuckets) {
    const map = {}

    for (let i = 0; i < costBuckets.length; i++) {
      const bucket = costBuckets[i] || {}
      const day = adminDailyBucket(map, bucket.starting_at, bucket.ending_at)
      if (!day) continue
      const results = Array.isArray(bucket.results) ? bucket.results : []
      for (let j = 0; j < results.length; j++) {
        const result = results[j] || {}
        const amount = numberValue(result.amount)
        const costUSD = amount === null ? 0 : amount / 100
        day.costUSD += costUSD
        const name = displayName(result.description || result.cost_type, "Claude API")
        day.costItems[name] = (day.costItems[name] || 0) + costUSD
      }
    }

    for (let i = 0; i < messageBuckets.length; i++) {
      const bucket = messageBuckets[i] || {}
      const day = adminDailyBucket(map, bucket.starting_at, bucket.ending_at)
      if (!day) continue
      const results = Array.isArray(bucket.results) ? bucket.results : []
      for (let j = 0; j < results.length; j++) {
        const result = results[j] || {}
        const input = intValue(result.uncached_input_tokens)
        const cacheCreation = cacheCreationTokenCount(result.cache_creation)
        const cacheRead = intValue(result.cache_read_input_tokens)
        const output = intValue(result.output_tokens)
        const total = input + cacheCreation + cacheRead + output
        day.inputTokens += input
        day.cacheCreationTokens += cacheCreation
        day.cacheReadTokens += cacheRead
        day.outputTokens += output
        day.totalTokens += total
        addAdminModel(day, displayName(result.model, "Claude API"), input, cacheCreation, cacheRead, output, total)
      }
    }

    return Object.keys(map)
      .sort()
      .map((key) => {
        const day = map[key]
        const modelBreakdowns = Object.keys(day.models)
          .map((name) => day.models[name])
          .sort((a, b) => b.totalTokens - a.totalTokens || a.modelName.localeCompare(b.modelName))
        const costItems = Object.keys(day.costItems)
          .map((name) => ({ name, costUSD: day.costItems[name] }))
          .sort((a, b) => b.costUSD - a.costUSD || a.name.localeCompare(b.name))
        return {
          date: day.date,
          inputTokens: day.inputTokens,
          cacheCreationTokens: day.cacheCreationTokens,
          cacheReadTokens: day.cacheReadTokens,
          outputTokens: day.outputTokens,
          totalTokens: day.totalTokens,
          costUSD: day.costUSD,
          totalCost: day.costUSD,
          modelsUsed: modelBreakdowns.map((model) => model.modelName),
          modelBreakdowns,
          costItems,
        }
      })
  }

  function fetchClaudeAdminUsage(ctx, apiKey) {
    const costs = fetchClaudeAdminReport(ctx, apiKey, ADMIN_COST_REPORT_URL, "cost_report", "description")
    const messages = fetchClaudeAdminReport(ctx, apiKey, ADMIN_MESSAGES_USAGE_URL, "messages", "model")
    return makeClaudeAdminDaily(costs, messages)
  }

  function fmtTokens(n) {
    const abs = Math.abs(n)
    const sign = n < 0 ? "-" : ""
    const units = [
      { threshold: 1e9, divisor: 1e9, suffix: "B" },
      { threshold: 1e6, divisor: 1e6, suffix: "M" },
      { threshold: 1e3, divisor: 1e3, suffix: "K" },
    ]
    for (let i = 0; i < units.length; i++) {
      const unit = units[i]
      if (abs >= unit.threshold) {
        const scaled = abs / unit.divisor
        const formatted = scaled >= 10
          ? Math.round(scaled).toString()
          : scaled.toFixed(1).replace(/\.0$/, "")
        return sign + formatted + unit.suffix
      }
    }
    return sign + Math.round(abs).toString()
  }

  function dayKeyFromDate(date) {
    const year = date.getFullYear()
    const month = date.getMonth() + 1
    const day = date.getDate()
    return year + "-" + (month < 10 ? "0" : "") + month + "-" + (day < 10 ? "0" : "") + day
  }

  function dayKeyFromUsageDate(rawDate) {
    if (typeof rawDate !== "string") return null
    const value = rawDate.trim()
    if (!value) return null

    const isoMatch = value.match(/^(\d{4})-(\d{2})-(\d{2})$/)
    if (isoMatch) {
      return isoMatch[1] + "-" + isoMatch[2] + "-" + isoMatch[3]
    }

    const isoDatePrefixMatch = value.match(/^(\d{4})-(\d{2})-(\d{2})(?:[Tt\s]|$)/)
    if (isoDatePrefixMatch) {
      return isoDatePrefixMatch[1] + "-" + isoDatePrefixMatch[2] + "-" + isoDatePrefixMatch[3]
    }

    const compactMatch = value.match(/^(\d{4})(\d{2})(\d{2})$/)
    if (compactMatch) {
      return compactMatch[1] + "-" + compactMatch[2] + "-" + compactMatch[3]
    }

    const ms = Date.parse(value)
    if (!Number.isFinite(ms)) return null
    return dayKeyFromDate(new Date(ms))
  }

  function usageCostUsd(day) {
    if (!day || typeof day !== "object") return null

    if (day.totalCost != null) {
      const totalCost = Number(day.totalCost)
      if (Number.isFinite(totalCost)) return totalCost
    }

    if (day.costUSD != null) {
      const costUSD = Number(day.costUSD)
      if (Number.isFinite(costUSD)) return costUSD
    }

    return null
  }

  function costAndTokensLabel(data, opts) {
    const includeZeroTokens = !!(opts && opts.includeZeroTokens)
    const parts = []
    if (data.costUSD != null) parts.push("$" + data.costUSD.toFixed(2))
    if (data.tokens > 0 || (includeZeroTokens && data.tokens === 0)) {
      parts.push(fmtTokens(data.tokens) + " tokens")
    }
    return parts.join(" · ")
  }

  function usageDayLabel(rawDate) {
    const key = dayKeyFromUsageDate(rawDate)
    if (!key) return String(rawDate || "").slice(0, 10) || "Usage"
    const month = Number(key.slice(5, 7))
    const day = Number(key.slice(8, 10))
    return month + "/" + day
  }

  function collectUsageChartPoints(daily) {
    const hasCost = daily.some((day) => {
      const cost = usageCostUsd(day)
      return cost !== null && cost > 0
    })
    const points = []
    for (let i = 0; i < daily.length; i++) {
      const day = daily[i]
      const tokens = Number(day && day.totalTokens)
      if (!Number.isFinite(tokens) || tokens < 0) continue
      const key = dayKeyFromUsageDate(day.date)
      if (!key) continue
      const cost = usageCostUsd(day) || 0
      points.push({
        key: key,
        label: usageDayLabel(day.date),
        value: hasCost ? cost : tokens,
        valueLabel: hasCost
          ? "$" + cost.toFixed(2) + " · " + fmtTokens(tokens) + " tokens"
          : fmtTokens(tokens) + " tokens",
      })
    }
    return points
      .sort((a, b) => a.key.localeCompare(b.key))
      .slice(-31)
      .map((point) => ({
        label: point.label,
        value: point.value,
        valueLabel: point.valueLabel,
      }))
  }

  function pushUsageChartLine(lines, ctx, daily, note) {
    const points = collectUsageChartPoints(daily)
    if (points.length === 0) return
    lines.push(ctx.line.barChart({
      label: "Usage Trend",
      points: points,
      note: note || "Estimated from local Claude logs at API rates.",
      color: "#DE7356",
    }))
  }

  function persistUsageDaily(ctx, daily, displayName, source) {
    if (!ctx.host.usageDaily || typeof ctx.host.usageDaily.ingest !== "function") return
    if (!daily || !daily.length) return
    try {
      const payload = { displayName: displayName, daily: daily }
      if (source) payload.source = source
      ctx.host.usageDaily.ingest(payload)
    } catch (e) { /* ignore */ }
  }

  function pushDayUsageLine(lines, ctx, label, dayEntry) {
    const tokens = Number(dayEntry && dayEntry.totalTokens) || 0
    const cost = usageCostUsd(dayEntry)
    if (tokens > 0 || cost !== null) {
      lines.push(ctx.line.text({
        label: label,
        value: costAndTokensLabel(
          { tokens: tokens, costUSD: cost === null ? 0 : cost },
          { includeZeroTokens: true }
        )
      }))
      return
    }

    lines.push(ctx.line.text({
      label: label,
      value: costAndTokensLabel({ tokens: 0, costUSD: 0 }, { includeZeroTokens: true })
    }))
  }

  function summarizeUsageDaily(daily) {
    const summary = { tokens: 0, costUSD: 0, hasCost: false }
    const selectedDaily = (daily || []).slice(-30)
    for (let i = 0; i < selectedDaily.length; i++) {
      const day = selectedDaily[i]
      const dayTokens = Number(day && day.totalTokens)
      if (Number.isFinite(dayTokens)) summary.tokens += dayTokens
      const dayCost = usageCostUsd(day)
      if (dayCost !== null) {
        summary.costUSD += dayCost
        summary.hasCost = true
      }
    }
    return summary
  }

  function appendUsageDailyLines(lines, ctx, daily, chartNote) {
    if (!daily || !daily.length) return false
    const now = new Date()
    const todayKey = dayKeyFromDate(now)
    const yesterday = new Date(now.getTime())
    yesterday.setDate(yesterday.getDate() - 1)
    const yesterdayKey = dayKeyFromDate(yesterday)

    let todayEntry = null
    let yesterdayEntry = null
    for (let i = 0; i < daily.length; i++) {
      const usageDayKey = dayKeyFromUsageDate(daily[i].date)
      if (usageDayKey === todayKey) {
        todayEntry = daily[i]
        continue
      }
      if (usageDayKey === yesterdayKey) {
        yesterdayEntry = daily[i]
      }
    }

    pushDayUsageLine(lines, ctx, "Today", todayEntry)
    pushDayUsageLine(lines, ctx, "Yesterday", yesterdayEntry)

    const summary = summarizeUsageDaily(daily)
    if (summary.tokens > 0 || summary.hasCost) {
      lines.push(ctx.line.text({
        label: "Last 30 Days",
        value: costAndTokensLabel({
          tokens: summary.tokens,
          costUSD: summary.hasCost ? summary.costUSD : null,
        }, { includeZeroTokens: true })
      }))
    }

    pushUsageChartLine(lines, ctx, daily, chartNote)
    return summary.tokens > 0 || summary.hasCost
  }

  // ── Web mode helpers ───────────────────────────────────────────────────────

  function extractSessionKey(value) {
    if (!value) return null
    const raw = String(value).trim()
    if (!raw) return null
    const cookieMatch = raw.match(/(?:^|;\s*)sessionKey=([^;]+)/i)
    if (cookieMatch && cookieMatch[1]) return cookieMatch[1].trim() || null
    const bare = raw.replace(/^sessionKey=/i, "").trim()
    return bare || null
  }

  function providerCookieHeader(ctx) {
    if (ctx.provider && typeof ctx.provider.cookieHeader === "string") {
      return ctx.provider.cookieHeader
    }
    return null
  }

  function getSessionKey(ctx) {
    const candidates = [
      readEnvText(ctx, "CLAUDE_AI_SESSION_KEY"),
      readEnvText(ctx, "CLAUDE_WEB_SESSION_KEY"),
      providerCookieHeader(ctx),
    ]
    for (let i = 0; i < candidates.length; i++) {
      const key = extractSessionKey(candidates[i])
      if (key) return key
    }
    return null
  }

  function buildWebHeaders(sessionKey) {
    return {
      Cookie: "sessionKey=" + sessionKey,
      Accept: "application/json",
      Origin: "https://claude.ai",
      Referer: "https://claude.ai/settings/usage",
      "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36",
      "anthropic-client-platform": "web_claude_ai",
    }
  }

  function fetchWebUsage(ctx, sessionKey) {
    const headers = buildWebHeaders(sessionKey)

    let orgResp
    try {
      orgResp = ctx.util.request({
        method: "GET",
        url: CLAUDE_WEB_BASE_URL + "/organizations",
        headers: headers,
        timeoutMs: 10000,
      })
    } catch (e) {
      ctx.host.log.error("web mode: org request failed: " + String(e))
      throw "Web usage request failed. Check your connection."
    }

    if (orgResp.status === 401 || orgResp.status === 403) {
      throw "Web session invalid or expired. Set CLAUDE_AI_SESSION_KEY to your claude.ai sessionKey cookie."
    }
    if (orgResp.status < 200 || orgResp.status >= 300) {
      throw "Web usage request failed (HTTP " + orgResp.status + "). Try again later."
    }

    const orgs = ctx.util.tryParseJson(orgResp.bodyText)
    if (!Array.isArray(orgs) || orgs.length === 0) {
      throw "No Claude organizations found in web response."
    }
    let orgId = null
    let usageData = null
    let lastUsageStatus = null
    for (let i = 0; i < orgs.length; i++) {
      const candidateOrgId = orgs[i] && orgs[i].uuid
      if (!candidateOrgId) continue

      let usageResp
      try {
        usageResp = ctx.util.request({
          method: "GET",
          url: CLAUDE_WEB_BASE_URL + "/organizations/" + candidateOrgId + "/usage",
          headers: headers,
          timeoutMs: 10000,
        })
      } catch (e) {
        ctx.host.log.error("web mode: usage request failed: " + String(e))
        throw "Web usage request failed. Check your connection."
      }

      lastUsageStatus = usageResp.status
      if (usageResp.status === 401 || usageResp.status === 403) {
        ctx.host.log.info("web mode: usage unavailable for org " + candidateOrgId + " (HTTP " + usageResp.status + ")")
        continue
      }
      if (usageResp.status < 200 || usageResp.status >= 300) {
        throw "Web usage request failed (HTTP " + usageResp.status + "). Try again later."
      }

      const parsedUsageData = ctx.util.tryParseJson(usageResp.bodyText)
      if (!parsedUsageData) throw "Web usage response invalid. Try again later."
      orgId = candidateOrgId
      usageData = parsedUsageData
      break
    }

    if (!usageData) {
      if (lastUsageStatus === 401) {
        throw "Web session invalid or expired. Set CLAUDE_AI_SESSION_KEY to your claude.ai sessionKey cookie."
      }
      if (lastUsageStatus === 403) {
        throw "No Claude organization with usage access found for this web session."
      }
      throw "Organization UUID missing in web response."
    }

    let accountInfo = null
    try {
      const accResp = ctx.util.request({
        method: "GET",
        url: CLAUDE_WEB_BASE_URL + "/account",
        headers: headers,
        timeoutMs: 5000,
      })
      if (accResp.status >= 200 && accResp.status < 300) {
        accountInfo = ctx.util.tryParseJson(accResp.bodyText)
      }
    } catch {
      // Account info is optional
    }

    ctx.host.log.info("web mode: usage fetched for org " + orgId)
    return { usageData: usageData, accountInfo: accountInfo }
  }

  function loadWebUsageData(ctx, sessionKey, nowMs) {
    const liveCache = readLiveUsageCache(ctx)
    const cachedLastWebUsageFetchMs = Number(liveCache && liveCache.lastWebUsageFetchMs) || 0
    const effectiveLastWebUsageFetchMs = Math.max(lastWebUsageFetchMs, cachedLastWebUsageFetchMs)
    const cached = cachedWebUsageData || cachedLiveUsageData(liveCache, nowMs)
    if (cached && nowMs - effectiveLastWebUsageFetchMs < MIN_USAGE_FETCH_INTERVAL_MS) {
      lastWebUsageFetchMs = effectiveLastWebUsageFetchMs
      ctx.host.log.info(
        "web usage fetch skipped: last fetch was " +
        Math.round((nowMs - effectiveLastWebUsageFetchMs) / 1000) + "s ago"
      )
      return { data: cached, plan: null }
    }

    lastWebUsageFetchMs = nowMs
    const result = fetchWebUsage(ctx, sessionKey)
    const validLines = []
    addUsageWindowLines(ctx, result.usageData, validLines)
    if (!validLines.length) throw "Web usage response invalid. Try again later."
    cachedWebUsageData = result.usageData
    writeLiveUsageCache(ctx, {
      usageData: result.usageData,
      usageFetchedAtMs: nowMs,
      lastWebUsageFetchMs: nowMs,
    })
    let plan = null
    if (result.accountInfo && result.accountInfo.rate_limit_tier) {
      plan = webTierToPlanLabel(result.accountInfo.rate_limit_tier)
    }
    return { data: result.usageData, plan: plan }
  }

  function webTierToPlanLabel(tier) {
    const t = (tier || "").toLowerCase()
    if (t === "free") return "Claude Free"
    if (t === "pro" || t === "claude_pro") return "Claude Pro"
    if (t === "claude_max_5" || t === "claude_max_5x") return "Claude Max 5x"
    if (t === "claude_max_20" || t === "claude_max_20x") return "Claude Max 20x"
    if (t === "max") return "Claude Max"
    if (t === "team") return "Claude Team"
    if (t === "enterprise") return "Claude Enterprise"
    return "Claude (" + tier + ")"
  }

  // ── Usage window rendering ─────────────────────────────────────────────────

  function pickUsageWindow(data, aliases) {
    for (let i = 0; i < aliases.length; i++) {
      const w = data[aliases[i]]
      if (w && typeof w === "object") return w
    }
    return null
  }

  function moneyMajorUnits(value) {
    if (typeof value !== "number" && typeof value !== "string") return null
    if (typeof value === "string" && !value.trim()) return null
    const n = Number(value)
    if (!Number.isFinite(n)) return null
    // Claude usage APIs return Extra usage amounts in minor units (cents).
    return Math.round(n) / 100
  }

  function currencySymbol(code) {
    const c = String(code || "").trim().toUpperCase()
    if (c === "EUR") return "€"
    if (c === "GBP") return "£"
    if (c === "JPY") return "¥"
    return "$"
  }

  function formatMoneyAmount(value, code) {
    if (typeof value !== "number" || !Number.isFinite(value)) return ""
    const symbol = currencySymbol(code)
    return symbol + value.toFixed(2).replace(/\.00$/, "")
  }

  function nextMonthResetIso(nowIso) {
    const now = new Date(nowIso || Date.now())
    if (Number.isNaN(now.getTime())) return null
    return new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1, 0, 0, 0)).toISOString()
  }

  function formatMonthDay(iso) {
    if (!iso) return null
    const d = new Date(iso)
    if (Number.isNaN(d.getTime())) return null
    const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
    return months[d.getUTCMonth()] + " " + String(d.getUTCDate())
  }

  function currentUsageWindow(data, kind, fallback) {
    const entries = Array.isArray(data.limits) ? data.limits.filter((entry) =>
      entry && entry.kind === kind && Number.isFinite(entry.percent) &&
      !(entry.scope && (entry.scope.model || entry.scope.surface))) : []
    const entry = entries.find((item) => item.is_active === true) || entries[0]
    return entry ? { utilization: entry.percent, resets_at: entry.resets_at } : fallback
  }

  function addUsageWindowLines(ctx, data, lines) {
    const settings = ctx.provider && ctx.provider.settings || {}
    const fiveHour = currentUsageWindow(data, "session", data.five_hour)
    if (fiveHour && Number.isFinite(fiveHour.utilization)) {
      lines.push(ctx.line.progress({
        label: "Session",
        used: fiveHour.utilization,
        limit: 100,
        format: { kind: "percent" },
        resetsAt: ctx.util.toIso(fiveHour.resets_at),
        periodDurationMs: 5 * 60 * 60 * 1000,
      }))
    }

    const sevenDay = currentUsageWindow(data, "weekly_all", data.seven_day)
    if (sevenDay && Number.isFinite(sevenDay.utilization)) {
      lines.push(ctx.line.progress({
        label: "Weekly",
        used: sevenDay.utilization,
        limit: 100,
        format: { kind: "percent" },
        resetsAt: ctx.util.toIso(sevenDay.resets_at),
        periodDurationMs: 7 * 24 * 60 * 60 * 1000,
      }))
    }

    const sevenDaySonnet = data.seven_day_sonnet
    if (settings.showModelQuotas === true && sevenDaySonnet && Number.isFinite(sevenDaySonnet.utilization)) {
      lines.push(ctx.line.progress({
        label: "Sonnet",
        used: sevenDaySonnet.utilization,
        limit: 100,
        format: { kind: "percent" },
        resetsAt: ctx.util.toIso(sevenDaySonnet.resets_at),
        periodDurationMs: 7 * 24 * 60 * 60 * 1000,
      }))
    }

    const sevenDayOpus = data.seven_day_opus
    if (settings.showModelQuotas === true && sevenDayOpus && Number.isFinite(sevenDayOpus.utilization)) {
      lines.push(ctx.line.progress({
        label: "Opus",
        used: sevenDayOpus.utilization,
        limit: 100,
        format: { kind: "percent" },
        resetsAt: ctx.util.toIso(sevenDayOpus.resets_at),
        periodDurationMs: 7 * 24 * 60 * 60 * 1000,
      }))
    }

    if (settings.showModelQuotas === true && Array.isArray(data.limits)) {
      for (const entry of data.limits) {
        if (!entry || entry.kind !== "weekly_scoped" || !Number.isFinite(entry.percent)) continue
        const model = entry.scope && entry.scope.model && entry.scope.model.display_name
        if (typeof model !== "string" || !model.trim() || lines.some((line) => line.label === model.trim())) continue
        lines.push(ctx.line.progress({ label: model.trim(), used: entry.percent, limit: 100,
          format: { kind: "percent" }, resetsAt: ctx.util.toIso(entry.resets_at), periodDurationMs: 7 * 24 * 60 * 60 * 1000 }))
      }
    }

    // Routines: check canonical name first, then legacy aliases
    const routinesWindow = pickUsageWindow(data, [
      "seven_day_routines",
      "seven_day_cowork",
      "seven_day_claude_routines",
    ])
    if (settings.hideDailyRoutines !== true && routinesWindow && Number.isFinite(routinesWindow.utilization)) {
      lines.push(ctx.line.progress({
        label: "Routines",
        used: routinesWindow.utilization,
        limit: 100,
        format: { kind: "percent" },
        resetsAt: ctx.util.toIso(routinesWindow.resets_at),
        periodDurationMs: 7 * 24 * 60 * 60 * 1000,
      }))
    }

    if (data.extra_usage && data.extra_usage.is_enabled) {
      const used = moneyMajorUnits(data.extra_usage.used_credits)
      const limit = moneyMajorUnits(data.extra_usage.monthly_limit)
      const currency = data.extra_usage.currency || data.extra_usage.currency_code || data.extra_usage.currencyCode || "USD"
      const resetIso = ctx.util.toIso(
        data.extra_usage.resets_at ||
        data.extra_usage.reset_at ||
        data.extra_usage.resetsAt ||
        data.extra_usage.resetAt ||
        data.extra_usage.monthly_reset_at ||
        data.extra_usage.monthlyResetAt ||
        data.extra_usage.period_end ||
        data.extra_usage.periodEnd ||
        data.extra_usage.billing_cycle_end ||
        data.extra_usage.billingCycleEnd
      ) || nextMonthResetIso(ctx.nowIso)
      const resetText = formatMonthDay(resetIso)

      if (typeof used === "number" && typeof limit === "number" && limit > 0) {
        const pct = Math.round((used / limit) * 100)
        let value = formatMoneyAmount(used, currency) + " / " + formatMoneyAmount(limit, currency)
        value += " (" + pct + "% used"
        if (resetText) value += ", resets " + resetText
        value += ")"
        lines.push(ctx.line.text({ label: "Extra usage spent", value: value }))
      } else if (typeof used === "number" && used > 0) {
        let value = formatMoneyAmount(used, currency)
        if (resetText) value += " (resets " + resetText + ")"
        lines.push(ctx.line.text({ label: "Extra usage spent", value: value }))
      }
    }
  }

  function oauthPlanLabel(ctx, creds) {
    if (!creds || !creds.oauth || !creds.oauth.subscriptionType) return null
    const basePlan = ctx.fmt.planLabel(creds.oauth.subscriptionType)
    if (!basePlan) return null

    let tierSuffix = ""
    const rlt = String(creds.oauth.rateLimitTier || "")
    const tierMatch = rlt.match(/(\d+)x/)
    if (tierMatch) {
      tierSuffix = " " + tierMatch[1] + "x"
    }
    return basePlan + tierSuffix
  }

  // ── probe() ────────────────────────────────────────────────────────────────

  function probe(ctx) {
    const creds = loadCredentials(ctx)
    const sessionKey = getSessionKey(ctx)
    const adminApiKey = readClaudeAdminApiKey(ctx)
    const hasOAuth = !!(creds && creds.oauth && creds.oauth.accessToken && creds.oauth.accessToken.trim())
    const sourceMode = String(ctx.sourceMode || "auto").toLowerCase()
    const wantsAuto = sourceMode === "auto"
    const wantsWeb = sourceMode === "web"
    const wantsOAuth = sourceMode === "oauth"
    const wantsLocal = sourceMode === "local"
    const wantsApi = sourceMode === "api"

    if (wantsWeb && !sessionKey) {
      ctx.host.log.error("web mode requested but no session key found")
      throw "Web session missing. Set CLAUDE_AI_SESSION_KEY to your claude.ai sessionKey cookie."
    }
    if (wantsOAuth && !hasOAuth) {
      ctx.host.log.error("oauth mode requested but no OAuth credentials found")
      throw "Not logged in. Run `claude` to authenticate."
    }
    if (wantsApi && !adminApiKey) {
      ctx.host.log.error("api mode requested but no Claude Admin API key found")
      throw "Claude API usage needs an Anthropic Admin API key. Set ANTHROPIC_ADMIN_KEY."
    }
    const noLiveCredentials = !hasOAuth && !sessionKey && !adminApiKey
    bindUsageCache(ctx, creds, sessionKey)
    const nowMs = Date.now()
    const liveCache = readLiveUsageCache(ctx)
    const homePath = getClaudeHomeOverride(ctx)
    let data = null
    let quotaSource = null
    let lines = []
    let plan = null
    let rateLimited = false
    let retryAfterSeconds = null

    if (hasOAuth) {
      plan = oauthPlanLabel(ctx, creds)
    }

    if (wantsLocal) {
      ctx.host.log.info("local mode requested; skipping live usage fetch")
    } else if (wantsApi) {
      ctx.host.log.info("api mode requested; skipping live quota fetch")
    } else if (hasOAuth && !wantsWeb) {
      quotaSource = "oauth"
      // ── OAuth mode ───────────────────────────────────────────────────────
      let accessToken = creds.oauth.accessToken
      const canFetchLiveUsage = hasProfileScope(creds)

      if (canFetchLiveUsage) {
        const cachedData = cachedUsageData || cachedLiveUsageData(liveCache, nowMs)
        const cachedRateLimitedUntilMs = Number(liveCache && liveCache.rateLimitedUntilMs) || 0
        const effectiveRateLimitedUntilMs = Math.max(rateLimitedUntilMs, cachedRateLimitedUntilMs)

        if (nowMs < effectiveRateLimitedUntilMs) {
          // Still within a rate-limit window from a previous probe call — skip the
          // API request entirely and surface the remaining wait time to the user.
          rateLimited = true
          rateLimitedUntilMs = effectiveRateLimitedUntilMs
          retryAfterSeconds = Math.ceil((effectiveRateLimitedUntilMs - nowMs) / 1000)
          data = cachedData
          ctx.host.log.info("usage fetch skipped: rate-limited for " + retryAfterSeconds + "s more")
        } else {
          // Rate-limit window has expired (or was never set).  Check whether we were
          // previously rate-limited so we can bypass the min-interval guard: a short
          // Retry-After (< 5 min) must not be swallowed by the normal poll throttle.
          const wasRateLimited = effectiveRateLimitedUntilMs > 0
          rateLimitedUntilMs = 0

          const cachedLastUsageFetchMs = Number(liveCache && liveCache.lastUsageFetchMs) || 0
          const effectiveLastUsageFetchMs = Math.max(lastUsageFetchMs, cachedLastUsageFetchMs)
          if (cachedData && !wasRateLimited && nowMs - effectiveLastUsageFetchMs < MIN_USAGE_FETCH_INTERVAL_MS) {
            // Polled too recently in normal operation — reuse last cached response.
            lastUsageFetchMs = effectiveLastUsageFetchMs
            data = cachedData
            ctx.host.log.info(
              "usage fetch skipped: last fetch was " +
              Math.round((nowMs - effectiveLastUsageFetchMs) / 1000) + "s ago (min interval " +
              MIN_USAGE_FETCH_INTERVAL_MS / 1000 + "s)"
            )
          } else {
            // Proactively refresh if token is expired or about to expire
            if (needsRefresh(ctx, creds.oauth, nowMs)) {
              ctx.host.log.info("token needs refresh (expired or expiring soon)")
              const refreshed = refreshToken(ctx, creds)
              if (refreshed) {
                accessToken = refreshed
              } else {
                ctx.host.log.warn("proactive refresh failed, trying with existing token")
              }
            }

            lastUsageFetchMs = nowMs
            let resp
            let didRefresh = false
            try {
              resp = ctx.util.retryOnceOnAuth({
                request: (token) => {
                  try {
                    return fetchUsage(ctx, token || accessToken)
                  } catch (e) {
                    ctx.host.log.error("usage request exception: " + String(e))
                    if (didRefresh) {
                      throw "Usage request failed after refresh. Try again."
                    }
                    throw "Usage request failed. Check your connection."
                  }
                },
                refresh: () => {
                  ctx.host.log.info("usage returned 401, attempting refresh")
                  didRefresh = true
                  return refreshToken(ctx, creds)
                },
              })
            } catch (e) {
              if (typeof e === "string") throw e
              ctx.host.log.error("usage request failed: " + String(e))
              throw "Usage request failed. Check your connection."
            }

            if (ctx.util.isAuthStatus(resp.status)) {
              ctx.host.log.error("usage returned auth error after all retries: status=" + resp.status)
              throw "Token expired. Run `claude` to log in again."
            }

            if (resp.status === 429) {
              rateLimited = true
              retryAfterSeconds = parseRetryAfterSeconds(resp.headers)
              const backoffMs = retryAfterSeconds !== null
                ? retryAfterSeconds * 1000
                : DEFAULT_RATE_LIMIT_BACKOFF_MS
              rateLimitedUntilMs = nowMs + backoffMs
              data = cachedData
              writeLiveUsageCache(ctx, {
                rateLimitedUntilMs: rateLimitedUntilMs,
                lastUsageFetchMs: nowMs,
                usageData: data,
                usageFetchedAtMs: Number(liveCache && liveCache.usageFetchedAtMs) || null,
                lastRateLimitedAtMs: nowMs,
              })
              ctx.host.log.warn(
                "usage rate limited (429), backing off for " +
                Math.round(backoffMs / 1000) + "s"
              )
            } else if (resp.status < 200 || resp.status >= 300) {
              ctx.host.log.error("usage returned error: status=" + resp.status)
              throw "Usage request failed (HTTP " + String(resp.status) + "). Try again later."
            } else {
              ctx.host.log.info("usage fetch succeeded")
              data = ctx.util.tryParseJson(resp.bodyText)
              const validLines = []
              if (data && typeof data === "object") addUsageWindowLines(ctx, data, validLines)
              if (!validLines.length) {
                throw "Usage response invalid. Try again later."
              }
              cachedUsageData = data
              rateLimitedUntilMs = 0
              writeLiveUsageCache(ctx, {
                rateLimitedUntilMs: 0,
                lastUsageFetchMs: nowMs,
                usageData: data,
                usageFetchedAtMs: nowMs,
                lastRateLimitedAtMs: null,
              })
            }
          } // end fetch else-branch
        }
      } else {
        ctx.host.log.info("skipping live usage fetch for inference-only token")
      }
    } else if (sessionKey) {
      // ── Web mode ─────────────────────────────────────────────────────────
      const webResult = loadWebUsageData(ctx, sessionKey, nowMs)
      data = webResult.data
      quotaSource = "web"
      if (webResult.plan) {
        plan = webResult.plan
      }
    } else {
      ctx.host.log.info("auto mode using available Claude usage history")
    }

    if (rateLimited && wantsAuto && sessionKey) {
      try {
        ctx.host.log.info("OAuth usage rate limited; trying Claude web fallback")
        const webResult = loadWebUsageData(ctx, sessionKey, nowMs)
        if (webResult.data) {
          data = webResult.data
          quotaSource = "web"
          rateLimited = false
          retryAfterSeconds = null
          if (webResult.plan) {
            plan = webResult.plan
          }
        }
      } catch (e) {
        ctx.host.log.warn("Claude web fallback failed after OAuth rate limit: " + String(e))
      }
    }

    // ── Render usage windows ─────────────────────────────────────────────────
    if (data) {
      addUsageWindowLines(ctx, data, lines)
    }

    // ── Token and cost history ───────────────────────────────────────────────
    let attachedUsageDaily = false
    if (!wantsLocal && adminApiKey && (wantsAuto || wantsApi)) {
      try {
        const adminDaily = fetchClaudeAdminUsage(ctx, adminApiKey)
        const summary = summarizeUsageDaily(adminDaily)
        if (summary.hasCost || summary.tokens > 0) {
          lines.push(ctx.line.text({
            label: "API Spend",
            value: summary.hasCost ? "$" + summary.costUSD.toFixed(2) : "$0.00",
          }))
        }
        attachedUsageDaily = appendUsageDailyLines(
          lines,
          ctx,
          adminDaily,
          "Claude Admin API organization usage."
        )
        persistUsageDaily(ctx, adminDaily, "Claude", "admin_billing")
      } catch (e) {
        const message = e && e.message ? e.message : String(e)
        if (wantsApi) throw message
        ctx.host.log.warn("Claude Admin API usage failed: " + message)
      }
    }

    if (!attachedUsageDaily && !wantsApi) {
      try {
        const usageResult = queryTokenUsage(ctx, homePath)
        if (usageResult.status === "ok") {
          const usage = usageResult.data
          appendUsageDailyLines(lines, ctx, usage.daily, "Estimated from local Claude logs at API rates.")
          persistUsageDaily(ctx, usage.daily, "Claude")
        }
      } catch (e) {
        ctx.host.log.warn("local Claude token usage failed: " + String(e))
      }
    }

    if (rateLimited) {
      const retryText = retryAfterSeconds !== null
        ? fmtRateLimitMinutes(retryAfterSeconds)
        : null
      const waitText = retryText
        ? "Rate limited, retry in ~" + retryText
        : "Rate limited, try again later"
      lines.unshift(ctx.line.badge({ label: "Status", text: waitText, color: "#f59e0b" }))
      const noteText = retryText
        ? "Live usage rate limited — retry in ~" + retryText
        : "Live usage rate limited — data may be stale"
      lines.push(ctx.line.text({ label: "Note", value: noteText }))
    } else if (lines.length === 0) {
      if (noLiveCredentials && wantsAuto) throw "Not logged in and no local Claude usage found. Run `claude` to authenticate."
      lines.push(ctx.line.badge({ label: "Status", text: "No usage data", color: "#a3a3a3" }))
    }

    const finalCache = readLiveUsageCache(ctx)
    const fetchedAt = data && finalCache && ctx.util.toIso(finalCache.usageFetchedAtMs)
    return { plan: plan, lines: lines, fetchedAt: fetchedAt || ctx.nowIso,
      source: data ? (rateLimited ? "cached" : quotaSource) : wantsApi ? "api" : "local" }
  }

  // _resetState is a testing hook — resets module-scope rate-limit state between tests.
  // The production host never calls this.
  function _resetState() {
    rateLimitedUntilMs = 0
    lastUsageFetchMs = 0
    cachedUsageData = null
    lastWebUsageFetchMs = 0
    cachedWebUsageData = null
  }

  globalThis.__openusage_plugin = { id: "claude", probe, _resetState }
})()
