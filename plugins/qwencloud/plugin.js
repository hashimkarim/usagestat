(function () {
  var GATEWAY_BASE_URL = "https://home.qwencloud.com";
  var DATA_GATEWAY_BASE_URL = "https://cs-data.qwencloud.com";
  var DASHBOARD_URL = "https://home.qwencloud.com/billing/subscription/token-plan-individual";
  var PRODUCT_CODE = "sfm_tokenplansolo_public_intl";
  var CONSOLE_PRODUCT = "sfm_bailian";
  var CONSOLE_ACTION = "IntlBroadScopeAspnGateway";
  var USAGE_API = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage";
  var SUBSCRIPTION_API = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/subscription";
  var QUOTA_CONFIG_API = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/quota-config";
  var REGION = "ap-southeast-1";
  var LANGUAGE = "en-US";
  var USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
  var FIVE_HOUR_MS = 5 * 60 * 60 * 1000;
  var WEEKLY_MS = 7 * 24 * 60 * 60 * 1000;
  var LEGACY_MS = 30 * 24 * 60 * 60 * 1000;

  var PLAN_NAME_KEYS = [
    "planName", "plan_name", "packageName", "package_name", "commodityName", "commodity_name",
    "instanceName", "instance_name", "displayName", "display_name", "name", "title",
    "planType", "plan_type", "ProductName", "productName", "InstanceCode",
  ];
  var USED_QUOTA_KEYS = [
    "usedQuota", "used_quota", "usedCredits", "usedCredit", "consumedCredits", "usage", "used",
    "usedAmount", "consumeAmount", "usedValue", "UsedValue", "consumedValue", "ConsumedValue",
  ];
  var TOTAL_QUOTA_KEYS = [
    "totalQuota", "total_quota", "totalCredits", "totalCredit", "quota", "creditLimit",
    "creditsTotal", "monthlyTotalQuota", "amount", "totalValue", "TotalValue", "CycleTotalValue",
    "cycleTotalValue", "subscriptionTotalNumber", "SubscriptionTotalNumber",
  ];
  var REMAINING_QUOTA_KEYS = [
    "remainingQuota", "remainQuota", "remainingCredits", "remainingCredit", "availableCredits",
    "balance", "remaining", "availableAmount", "remainAmount", "totalSurplusValue",
    "TotalSurplusValue", "surplusValue", "SurplusValue", "CycleSurplusValue", "cycleSurplusValue",
  ];
  var RESET_DATE_KEYS = [
    "nextRefreshTime", "resetTime", "periodEndTime", "billingCycleEnd", "billCycleEndTime",
    "expireTime", "expirationTime", "endTime", "EndTime", "validEndTime", "instanceEndTime",
    "nearestExpireDate", "NearestExpireDate",
  ];

  function trim(value) {
    return typeof value === "string" && value.trim() ? value.trim() : null;
  }

  function clean(value) {
    var text = trim(value);
    if (!text) return null;
    if ((text[0] === "\"" && text[text.length - 1] === "\"") || (text[0] === "'" && text[text.length - 1] === "'")) text = text.slice(1, -1).trim();
    return text || null;
  }

  function env(ctx, name) {
    try {
      return clean(ctx.host.env.get(name));
    } catch (_) {
      return null;
    }
  }

  function setting(ctx, names) {
    var settings = ctx.provider && ctx.provider.settings ? ctx.provider.settings : {};
    for (var i = 0; i < names.length; i++) {
      var value = clean(settings[names[i]]);
      if (value) return value;
    }
    return null;
  }

  function normalizeCookie(raw) {
    var text = clean(raw);
    if (!text) return null;
    if (text.slice(0, 7).toLowerCase() === "cookie:") text = text.slice(7).trim();
    return text && text.indexOf("=") >= 0 ? text : null;
  }

  function cookieHeader(ctx) {
    var value = normalizeCookie(ctx.provider && ctx.provider.cookieHeader) ||
      normalizeCookie(setting(ctx, ["cookieHeader", "cookie"])) ||
      normalizeCookie(env(ctx, "QWEN_CLOUD_COOKIE")) ||
      normalizeCookie(env(ctx, "QWEN_CLOUD_COOKIE_HEADER"));
    if (!value) throw "Qwen Cloud session not configured. Set QWEN_CLOUD_COOKIE or provider cookieHeader.";
    return value;
  }

  function cookieValue(name, cookie) {
    var parts = String(cookie || "").split(";");
    for (var i = 0; i < parts.length; i++) {
      var idx = parts[i].indexOf("=");
      if (idx < 0) continue;
      var key = parts[i].slice(0, idx).trim();
      var value = parts[i].slice(idx + 1).trim();
      if (key.toLowerCase() === name.toLowerCase() && value) return value;
    }
    return null;
  }

  function extractSecToken(html) {
    var patterns = [
      /"secToken"\s*:\s*"([^"]+)"/,
      /"sec_token"\s*:\s*"([^"]+)"/,
      /secToken['"]?\s*[:=]\s*['"]([^'"]+)['"]/,
      /sec_token['"]?\s*[:=]\s*['"]([^'"]+)['"]/,
      /csrfToken['"]?\s*[:=]\s*['"]([^'"]+)['"]/,
    ];
    for (var i = 0; i < patterns.length; i++) {
      var match = patterns[i].exec(String(html || ""));
      if (match && match[1] && match[1].trim()) return match[1].trim();
    }
    return null;
  }

  function looksLikeLogin(text) {
    var lower = String(text || "").toLowerCase();
    return lower.indexOf("passport.alibabacloud.com") >= 0 ||
      lower.indexOf("signin.aliyun.com") >= 0 ||
      lower.indexOf("account.alibabacloud.com/login") >= 0 ||
      lower.indexOf("login.qwencloud.com") >= 0 ||
      (lower.indexOf("login") >= 0 && lower.indexOf("password") >= 0 && lower.indexOf("sign in") >= 0);
  }

  function expandJsonStrings(value) {
    if (Array.isArray(value)) return value.map(expandJsonStrings);
    if (value && typeof value === "object") {
      var out = {};
      Object.keys(value).forEach(function (key) { out[key] = expandJsonStrings(value[key]); });
      return out;
    }
    if (typeof value === "string") {
      var text = value.trim();
      if ((text[0] === "{" && text[text.length - 1] === "}") || (text[0] === "[" && text[text.length - 1] === "]")) {
        try {
          return expandJsonStrings(JSON.parse(text));
        } catch (_) {}
      }
    }
    return value;
  }

  function findFirstString(value, keys) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var found = findFirstString(value[i], keys);
        if (found) return found;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    for (var k = 0; k < keys.length; k++) {
      var raw = value[keys[k]];
      if (typeof raw === "string" && raw.trim()) return raw.trim();
    }
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findFirstString(value[names[j]], keys);
      if (nested) return nested;
    }
    return null;
  }

  function resolveSecToken(ctx, cookie) {
    try {
      var response = ctx.util.request({
        method: "GET",
        url: DASHBOARD_URL,
        headers: {
          Cookie: cookie,
          Accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
          "User-Agent": USER_AGENT,
        },
        timeoutMs: 10000,
      });
      if (response.status >= 200 && response.status < 300 && !looksLikeLogin(response.bodyText)) {
        var token = extractSecToken(response.bodyText);
        if (token) return token;
      }
    } catch (_) {}

    var fromCookie = cookieValue("sec_token", cookie);
    if (fromCookie) return fromCookie;

    try {
      var userInfo = ctx.util.requestJson({
        method: "GET",
        url: GATEWAY_BASE_URL + "/tool/user/info.json",
        headers: {
          Cookie: cookie,
          Accept: "application/json, text/plain, */*",
          "User-Agent": USER_AGENT,
        },
        timeoutMs: 10000,
      });
      if (userInfo.resp.status >= 200 && userInfo.resp.status < 300 && userInfo.json) {
        var expanded = expandJsonStrings(userInfo.json);
        return findFirstString(expanded, ["secToken", "sec_token", "csrfToken", "token"]);
      }
    } catch (_) {}

    return null;
  }

  function apiUrl(api) {
    return DATA_GATEWAY_BASE_URL + "/data/api.json?action=" + encodeURIComponent(CONSOLE_ACTION) +
      "&product=" + encodeURIComponent(CONSOLE_PRODUCT) +
      "&api=" + encodeURIComponent(api) +
      "&_v=undefined";
  }

  function randomId() {
    return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, function (ch) {
      var r = Math.floor(Math.random() * 16);
      var v = ch === "x" ? r : (r & 3) | 8;
      return v.toString(16);
    });
  }

  function paramsJson(api, data, cookie) {
    data = data || {};
    data.cornerstoneParam = {
      feTraceId: randomId(),
      feURL: DASHBOARD_URL,
      protocol: "V2",
      console: "ONE_CONSOLE",
      productCode: "p_efm",
      domain: "home.qwencloud.com",
      consoleSite: "QWENCLOUD",
      userNickName: "",
      userPrincipalName: "",
      xsp_lang: LANGUAGE,
    };
    var cna = cookieValue("cna", cookie);
    if (cna) data.cornerstoneParam["X-Anonymous-Id"] = cna;
    return JSON.stringify({ Api: api, V: "1.0", Data: data });
  }

  function formBody(parts) {
    return parts.map(function (part) {
      return encodeURIComponent(part[0]) + "=" + encodeURIComponent(part[1]);
    }).join("&");
  }

  function postApi(ctx, api, data, secToken, cookie, optional) {
    var headers = {
      Cookie: cookie,
      Accept: "application/json, text/plain, */*",
      "Content-Type": "application/x-www-form-urlencoded",
      Origin: GATEWAY_BASE_URL,
      Referer: DASHBOARD_URL,
      "User-Agent": USER_AGENT,
      "X-Requested-With": "XMLHttpRequest",
    };
    var csrf = cookieValue("login_aliyunid_csrf", cookie) || cookieValue("csrf", cookie);
    if (csrf) {
      headers["x-xsrf-token"] = csrf;
      headers["x-csrf-token"] = csrf;
    }
    var result = ctx.util.request({
      method: "POST",
      url: apiUrl(api),
      headers: headers,
      bodyText: formBody([
        ["product", CONSOLE_PRODUCT],
        ["action", CONSOLE_ACTION],
        ["sec_token", secToken],
        ["region", REGION],
        ["language", LANGUAGE],
        ["params", paramsJson(api, data, cookie)],
      ]),
      timeoutMs: 20000,
    });
    if (ctx.util.isAuthStatus(result.status)) throw "Qwen Cloud login required.";
    if (result.status < 200 || result.status >= 300) {
      if (optional) return null;
      throw "Qwen Cloud API error: HTTP " + result.status + ".";
    }
    var json = ctx.util.tryParseJson(result.bodyText);
    if (!json) {
      var lower = String(result.bodyText || "").toLowerCase();
      if (lower.indexOf("<html") >= 0 && (lower.indexOf("login") >= 0 || lower.indexOf("sign in") >= 0 || lower.indexOf("signin") >= 0)) {
        throw "Qwen Cloud login required.";
      }
      if (optional) return null;
      throw "Invalid Qwen Cloud JSON response.";
    }
    return expandJsonStrings(json);
  }

  function parseNumber(value) {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      var parsed = Number(value.replace(/,/g, ""));
      if (Number.isFinite(parsed)) return parsed;
    }
    return null;
  }

  function directNumber(obj, keys) {
    if (!obj || typeof obj !== "object" || Array.isArray(obj)) return null;
    for (var i = 0; i < keys.length; i++) {
      var value = parseNumber(obj[keys[i]]);
      if (value !== null) return value;
    }
    return null;
  }

  function directString(obj, keys) {
    if (!obj || typeof obj !== "object" || Array.isArray(obj)) return null;
    for (var i = 0; i < keys.length; i++) {
      var value = obj[keys[i]];
      if (typeof value === "string" && value.trim()) return value.trim();
    }
    return null;
  }

  function parseBool(value) {
    if (typeof value === "boolean") return value;
    if (typeof value === "number") return value !== 0;
    if (typeof value === "string") {
      var lower = value.trim().toLowerCase();
      if (/^(true|1|yes|active|valid|normal)$/.test(lower)) return true;
      if (/^(false|0|no|inactive|invalid|expired)$/.test(lower)) return false;
    }
    return null;
  }

  function findFirstNumber(value, keys) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var inArray = findFirstNumber(value[i], keys);
        if (inArray !== null) return inArray;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    var direct = directNumber(value, keys);
    if (direct !== null) return direct;
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findFirstNumber(value[names[j]], keys);
      if (nested !== null) return nested;
    }
    return null;
  }

  function findFirstBool(value, keys) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var inArray = findFirstBool(value[i], keys);
        if (inArray !== null) return inArray;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    for (var k = 0; k < keys.length; k++) {
      var direct = parseBool(value[keys[k]]);
      if (direct !== null) return direct;
    }
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findFirstBool(value[names[j]], keys);
      if (nested !== null) return nested;
    }
    return null;
  }

  function throwIfErrorPayload(value) {
    var code = findFirstNumber(value, ["statusCode", "status_code", "code"]);
    if (code !== null && code !== 0 && code !== 200) {
      if (code === 401 || code === 403) throw "Qwen Cloud login required.";
      throw "Qwen Cloud API error: " + (findFirstString(value, ["statusMessage", "status_msg", "message", "msg"]) || "status code " + code);
    }
    var success = findFirstBool(value, ["success", "Success", "successResponse"]);
    if (success === false) {
      var message = findFirstString(value, ["message", "msg", "Message", "errorMessage"]) || "request failed";
      var lower = message.toLowerCase();
      if (lower.indexOf("login") >= 0 || lower.indexOf("unauthorized") >= 0) throw "Qwen Cloud login required.";
      throw "Qwen Cloud API error: " + message;
    }
  }

  function parseDate(ctx, value) {
    var n = parseNumber(value);
    if (n !== null && n > 1000000000) return ctx.util.toIso(n);
    var text = trim(value);
    if (!text) return null;
    if (/^\d{4}-\d{2}-\d{2}$/.test(text)) return ctx.util.toIso(text + "T00:00:00Z");
    if (/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}/.test(text)) return ctx.util.toIso(text.replace(" ", "T") + "Z");
    return ctx.util.toIso(text);
  }

  function findFirstDate(ctx, value, keys) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var inArray = findFirstDate(ctx, value[i], keys);
        if (inArray) return inArray;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    for (var k = 0; k < keys.length; k++) {
      var direct = parseDate(ctx, value[keys[k]]);
      if (direct) return direct;
    }
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findFirstDate(ctx, value[names[j]], keys);
      if (nested) return nested;
    }
    return null;
  }

  function findObjectContaining(value, keys) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var found = findObjectContaining(value[i], keys);
        if (found) return found;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    for (var k = 0; k < keys.length; k++) {
      if (Object.prototype.hasOwnProperty.call(value, keys[k])) return value;
    }
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findObjectContaining(value[names[j]], keys);
      if (nested) return nested;
    }
    return null;
  }

  function findObjectByKeys(value, keys) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var found = findObjectByKeys(value[i], keys);
        if (found) return found;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    for (var k = 0; k < keys.length; k++) {
      if (value[keys[k]] && typeof value[keys[k]] === "object" && !Array.isArray(value[keys[k]])) return value[keys[k]];
    }
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findObjectByKeys(value[names[j]], keys);
      if (nested) return nested;
    }
    return null;
  }

  function findArrayByKeys(value, keys) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var found = findArrayByKeys(value[i], keys);
        if (found) return found;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    for (var k = 0; k < keys.length; k++) {
      if (Array.isArray(value[keys[k]])) return value[keys[k]];
    }
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findArrayByKeys(value[names[j]], keys);
      if (nested) return nested;
    }
    return null;
  }

  function activeSignalScore(obj) {
    var status = String(directString(obj, ["status", "instanceStatus", "state", "Status"]) || "").toUpperCase();
    if (/^(VALID|ACTIVE|NORMAL)$/.test(status)) return 3;
    if (/^(EXPIRED|INVALID|INACTIVE|DISABLED|TERMINATED|STOPPED)$/.test(status)) return -1;
    var active = parseBool(obj && (obj.isActive !== undefined ? obj.isActive : obj.active));
    return active === null ? 0 : active ? 3 : -1;
  }

  function findTokenPlanInstance(value) {
    var object = findObjectByKeys(value, ["tokenPlanInstanceInfo", "token_plan_instance_info", "instanceInfo", "instance_info"]);
    if (object) return object;
    var array = findArrayByKeys(value, ["tokenPlanInstanceInfos", "token_plan_instance_infos", "instanceInfos", "instances", "EquityList", "Data", "data", "successResponse"]);
    if (!array) return null;
    var best = null;
    var bestScore = -999;
    for (var i = 0; i < array.length; i++) {
      if (!array[i] || typeof array[i] !== "object" || Array.isArray(array[i])) continue;
      var score = activeSignalScore(array[i]);
      if (!best || score > bestScore) {
        best = array[i];
        bestScore = score;
      }
    }
    return best;
  }

  function findObjectWithQuotaKeys(value) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var found = findObjectWithQuotaKeys(value[i]);
        if (found) return found;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    var all = USED_QUOTA_KEYS.concat(TOTAL_QUOTA_KEYS).concat(REMAINING_QUOTA_KEYS);
    for (var k = 0; k < all.length; k++) {
      if (Object.prototype.hasOwnProperty.call(value, all[k])) return value;
    }
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findObjectWithQuotaKeys(value[names[j]]);
      if (nested) return nested;
    }
    return null;
  }

  function planNameFromResponse(value) {
    var plan = findObjectContaining(value, ["specCode", "spec_code", "planName", "plan_name"]);
    if (!plan) return null;
    var code = directString(plan, ["specCode", "spec_code", "planName", "plan_name"]);
    if (!code) return null;
    var lower = code.toLowerCase();
    if (lower === "lite") return "Lite";
    if (lower === "standard") return "Standard";
    if (lower === "pro") return "Pro";
    if (lower === "max") return "Max";
    return code;
  }

  function quotaTotalsForPlan(quotaConfig, planName) {
    if (!quotaConfig || !planName) return { five: null, weekly: null };
    var target = planName.toLowerCase();
    var found = findFirstValueForKey(quotaConfig, target);
    if (!found || typeof found !== "object") return { five: null, weekly: null };
    return {
      five: parseNumber(found.five_hour) || parseNumber(found.fiveHour),
      weekly: parseNumber(found.weekly),
    };
  }

  function findFirstValueForKey(value, key) {
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) {
        var found = findFirstValueForKey(value[i], key);
        if (found !== null) return found;
      }
      return null;
    }
    if (!value || typeof value !== "object") return null;
    if (Object.prototype.hasOwnProperty.call(value, key)) return value[key];
    var names = Object.keys(value);
    for (var j = 0; j < names.length; j++) {
      var nested = findFirstValueForKey(value[names[j]], key);
      if (nested !== null) return nested;
    }
    return null;
  }

  function percentagePoints(value) {
    var n = parseNumber(value);
    if (n === null) return null;
    return Math.max(0, Math.min(100, Math.max(0, Math.min(1, n)) * 100));
  }

  function usedPercent(used, total, remaining) {
    if (total === null || total <= 0) return null;
    if (used === null && remaining !== null) used = total - remaining;
    if (used === null) return null;
    return Math.max(0, Math.min(100, Math.max(0, Math.min(total, used)) / total * 100));
  }

  function parseCurrent(ctx, usage, subscription, quotaConfig) {
    var record = findObjectContaining(usage, ["per5HourPercentage", "per1WeekPercentage"]);
    if (!record) return null;
    var five = percentagePoints(record.per5HourPercentage);
    var weekly = percentagePoints(record.per1WeekPercentage);
    if (five === null && weekly === null) return null;
    var plan = subscription ? planNameFromResponse(subscription) : null;
    var totals = quotaConfig ? quotaTotalsForPlan(quotaConfig, plan) : { five: null, weekly: null };
    return {
      planName: plan,
      fivePercent: five,
      fiveTotal: totals.five,
      fiveReset: parseDate(ctx, record.per5HourResetTime),
      weeklyPercent: weekly,
      weeklyTotal: totals.weekly,
      weeklyReset: parseDate(ctx, record.per1WeekResetTime),
    };
  }

  function parseLegacy(ctx, payload) {
    var instance = findTokenPlanInstance(payload);
    var scope = instance || payload;
    var quota = findObjectByKeys(scope, ["quotaInfo", "quota_info", "tokenPlanQuotaInfo", "token_plan_quota_info"]) ||
      findObjectWithQuotaKeys(scope) ||
      findObjectWithQuotaKeys(payload);
    var used = quota ? directNumber(quota, USED_QUOTA_KEYS) : null;
    var total = quota ? directNumber(quota, TOTAL_QUOTA_KEYS) : null;
    var remaining = quota ? directNumber(quota, REMAINING_QUOTA_KEYS) : null;
    if (used === null) used = findFirstNumber(scope, USED_QUOTA_KEYS);
    if (total === null) total = findFirstNumber(scope, TOTAL_QUOTA_KEYS);
    if (remaining === null) remaining = findFirstNumber(scope, REMAINING_QUOTA_KEYS);
    var percent = usedPercent(used, total, remaining);
    if (percent === null) throw "Qwen Cloud has no active token-plan subscription.";
    return {
      planName: directString(scope, PLAN_NAME_KEYS) || findFirstString(payload, PLAN_NAME_KEYS),
      used: used,
      total: total,
      remaining: remaining,
      percent: percent,
      reset: findFirstDate(ctx, scope, RESET_DATE_KEYS) || findFirstDate(ctx, payload, RESET_DATE_KEYS),
    };
  }

  function formatQuota(value) {
    if (!Number.isFinite(value)) return "";
    var rounded = Math.round(value);
    if (Math.abs(rounded - value) < 0.000001) return String(rounded).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
    return value.toFixed(2).replace(/\.?0+$/, "").replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  }

  function quotaDetail(percent, total) {
    if (!total || total <= 0) return null;
    return formatQuota(total * percent / 100) + " / " + formatQuota(total) + " credits used";
  }

  function progress(ctx, label, percent, periodMs, reset, detail) {
    var opts = {
      label: label,
      used: Math.max(0, Math.min(100, percent)),
      limit: 100,
      format: { kind: "percent" },
      periodDurationMs: periodMs,
    };
    if (reset) opts.resetsAt = reset;
    if (detail) opts.detail = detail;
    return ctx.line.progress(opts);
  }

  function snapshotToResult(ctx, snapshot) {
    var lines = [];
    if (snapshot.fivePercent !== undefined) {
      if (snapshot.fivePercent !== null) {
        lines.push(progress(ctx, "5-hour", snapshot.fivePercent, FIVE_HOUR_MS, snapshot.fiveReset, quotaDetail(snapshot.fivePercent, snapshot.fiveTotal)));
      } else if (snapshot.percent !== null && snapshot.percent !== undefined) {
        lines.push(progress(ctx, "Credits", snapshot.percent, LEGACY_MS, snapshot.reset, snapshot.used !== null && snapshot.total !== null ? formatQuota(snapshot.used) + " / " + formatQuota(snapshot.total) + " credits used" : null));
      }
      if (snapshot.weeklyPercent !== null && snapshot.weeklyPercent !== undefined) {
        lines.push(progress(ctx, "Weekly", snapshot.weeklyPercent, WEEKLY_MS, snapshot.weeklyReset, quotaDetail(snapshot.weeklyPercent, snapshot.weeklyTotal)));
      }
    } else {
      lines.push(progress(ctx, "Credits", snapshot.percent, LEGACY_MS, snapshot.reset, snapshot.used !== null && snapshot.total !== null ? formatQuota(snapshot.used) + " / " + formatQuota(snapshot.total) + " credits used" : null));
    }
    if (!lines.length && snapshot.weeklyPercent !== null && snapshot.weeklyPercent !== undefined) {
      lines.push(progress(ctx, "Weekly", snapshot.weeklyPercent, WEEKLY_MS, snapshot.weeklyReset, quotaDetail(snapshot.weeklyPercent, snapshot.weeklyTotal)));
    }
    if (!lines.length) throw "Qwen Cloud usage windows missing.";
    return { displayName: "Qwen Cloud", source: "web", plan: snapshot.planName, lines: lines };
  }

  function probe(ctx) {
    var cookie = cookieHeader(ctx);
    var secToken = resolveSecToken(ctx, cookie);
    if (!secToken) throw "Qwen Cloud login required.";
    var usage = postApi(ctx, USAGE_API, {}, secToken, cookie, false);
    throwIfErrorPayload(usage);
    var subscription = postApi(ctx, SUBSCRIPTION_API, { commodityCode: PRODUCT_CODE }, secToken, cookie, true);
    var quotaConfig = postApi(ctx, QUOTA_CONFIG_API, {}, secToken, cookie, true);
    var snapshot = parseCurrent(ctx, usage, subscription, quotaConfig) || parseLegacy(ctx, usage);
    return snapshotToResult(ctx, snapshot);
  }

  globalThis.__openusage_plugin = { id: "qwencloud", probe: probe };
})();
