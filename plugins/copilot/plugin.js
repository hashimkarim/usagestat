(function () {
  const KEYCHAIN_SERVICE = "OpenUsage-copilot";
  const GH_KEYCHAIN_SERVICE = "gh:github.com";
  const DEFAULT_USAGE_URL = "https://api.github.com/copilot_internal/user";
  const BUDGETS_URL = "https://github.com/settings/billing/budgets";
  const COPILOT_PRODUCT_ID = "copilot";
  const COPILOT_PREMIUM_REQUEST_SKU = "copilot_premium_request";
  const COPILOT_AGENT_PREMIUM_REQUEST_SKU = "copilot_agent_premium_request";
  const SPARK_PREMIUM_REQUEST_SKU = "spark_premium_request";
  const COPILOT_BUDGET_SELECTORS = [
    COPILOT_PRODUCT_ID,
    COPILOT_PREMIUM_REQUEST_SKU,
    COPILOT_AGENT_PREMIUM_REQUEST_SKU,
    SPARK_PREMIUM_REQUEST_SKU,
  ];

  function readJson(ctx, path) {
    try {
      if (!ctx.host.fs.exists(path)) return null;
      const text = ctx.host.fs.readText(path);
      return ctx.util.tryParseJson(text);
    } catch (e) {
      ctx.host.log.warn("readJson failed for " + path + ": " + String(e));
      return null;
    }
  }

  function writeJson(ctx, path, value) {
    try {
      ctx.host.fs.writeText(path, JSON.stringify(value));
    } catch (e) {
      ctx.host.log.warn("writeJson failed for " + path + ": " + String(e));
    }
  }

  function saveToken(ctx, token) {
    try {
      ctx.host.keychain.writeGenericPassword(
        KEYCHAIN_SERVICE,
        JSON.stringify({ token: token }),
      );
    } catch (e) {
      ctx.host.log.warn("keychain write failed: " + String(e));
    }
    writeJson(ctx, ctx.app.pluginDataDir + "/auth.json", { token: token });
  }

  function clearCachedToken(ctx) {
    try {
      ctx.host.keychain.deleteGenericPassword(KEYCHAIN_SERVICE);
    } catch (e) {
      ctx.host.log.info("keychain delete failed: " + String(e));
    }
    writeJson(ctx, ctx.app.pluginDataDir + "/auth.json", null);
  }

  function loadTokenFromKeychain(ctx) {
    try {
      const raw = ctx.host.keychain.readGenericPassword(KEYCHAIN_SERVICE);
      if (raw) {
        const parsed = ctx.util.tryParseJson(raw);
        if (parsed && parsed.token) {
          ctx.host.log.info("token loaded from OpenUsage keychain");
          return { token: parsed.token, source: "keychain" };
        }
      }
    } catch (e) {
      ctx.host.log.info("OpenUsage keychain read failed: " + String(e));
    }
    return null;
  }

  function loadTokenFromGhCli(ctx) {
    try {
      const raw = ctx.host.keychain.readGenericPassword(GH_KEYCHAIN_SERVICE);
      if (raw) {
        let token = raw;
        if (
          typeof token === "string" &&
          token.indexOf("go-keyring-base64:") === 0
        ) {
          token = ctx.base64.decode(token.slice("go-keyring-base64:".length));
        }
        if (token) {
          ctx.host.log.info("token loaded from gh CLI keychain");
          return { token: token, source: "gh-cli" };
        }
      }
    } catch (e) {
      ctx.host.log.info("gh CLI keychain read failed: " + String(e));
    }
    return null;
  }

  function loadTokenFromGhCommand(ctx) {
    try {
      if (!ctx.host.command || typeof ctx.host.command.run !== "function") return null;
      const result = ctx.host.command.run({
        program: "gh",
        args: ["auth", "token"],
        timeoutMs: 10000,
      });
      if (result && result.status === 0 && typeof result.stdout === "string") {
        const token = result.stdout.trim();
        if (token) {
          ctx.host.log.info("token loaded from gh auth token");
          return { token: token, source: "gh-command" };
        }
      }
      if (result && result.stderr) {
        ctx.host.log.info("gh auth token failed: " + String(result.stderr).trim());
      }
    } catch (e) {
      ctx.host.log.info("gh auth token command failed: " + String(e));
    }
    return null;
  }

  function loadTokenFromStateFile(ctx) {
    const data = readJson(ctx, ctx.app.pluginDataDir + "/auth.json");
    if (data && data.token) {
      ctx.host.log.info("token loaded from state file");
      return { token: data.token, source: "state" };
    }
    return null;
  }

  function loadTokenFromEnv(ctx) {
    const names = ["COPILOT_API_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];
    for (let i = 0; i < names.length; i += 1) {
      try {
        const value = ctx.host.env.get(names[i]);
        if (typeof value === "string" && value.trim()) {
          ctx.host.log.info("token loaded from " + names[i]);
          return { token: value.trim(), source: "env" };
        }
      } catch (e) {
        ctx.host.log.warn("env token read failed for " + names[i] + ": " + String(e));
      }
    }
    return null;
  }

  function loadToken(ctx) {
    return (
      loadTokenFromEnv(ctx) ||
      loadTokenFromKeychain(ctx) ||
      loadTokenFromGhCli(ctx) ||
      loadTokenFromGhCommand(ctx) ||
      loadTokenFromStateFile(ctx)
    );
  }

  function fetchUsage(ctx, token) {
    const usageUrl = ctx.host.env.get("COPILOT_USAGE_URL") || DEFAULT_USAGE_URL;
    return ctx.util.request({
      method: "GET",
      url: usageUrl,
      headers: {
        Authorization: "token " + token,
        Accept: "application/json",
        "Editor-Version": "vscode/1.96.2",
        "Editor-Plugin-Version": "copilot-chat/0.26.7",
        "User-Agent": "GitHubCopilotChat/0.26.7",
        "X-Github-Api-Version": "2025-04-01",
      },
      timeoutMs: 10000,
    });
  }

  function trimText(value) {
    if (value === null || value === undefined) return null;
    const text = String(value).trim();
    return text || null;
  }

  function readEnv(ctx, name) {
    try {
      return trimText(ctx.host.env.get(name));
    } catch (_) {
      return null;
    }
  }

  function providerSetting(ctx, names) {
    const settings = ctx.provider && ctx.provider.settings ? ctx.provider.settings : {};
    for (let i = 0; i < names.length; i += 1) {
      const value = trimText(settings[names[i]]);
      if (value) return value;
    }
    return null;
  }

  function normalizeCookieHeader(value) {
    let text = trimText(value);
    if (!text) return null;
    text = text.replace(/^Cookie:\s*/i, "").trim();
    return text || null;
  }

  function loadBudgetCookieHeader(ctx) {
    return normalizeCookieHeader(ctx.provider && ctx.provider.cookieHeader) ||
      normalizeCookieHeader(providerSetting(ctx, ["budgetCookieHeader", "githubCookieHeader", "cookieHeader"])) ||
      normalizeCookieHeader(readEnv(ctx, "COPILOT_BUDGET_COOKIE_HEADER")) ||
      normalizeCookieHeader(readEnv(ctx, "COPILOT_BUDGET_COOKIE")) ||
      normalizeCookieHeader(readEnv(ctx, "GITHUB_COOKIE_HEADER")) ||
      normalizeCookieHeader(readEnv(ctx, "GITHUB_COOKIE"));
  }

  function requestGitHubBudget(ctx, url, cookieHeader, accept, nonce) {
    const headers = {
      Cookie: cookieHeader,
      Accept: accept,
      Referer: BUDGETS_URL,
      "User-Agent": "CodexBar",
    };
    if (accept.indexOf("json") >= 0) {
      headers["X-Requested-With"] = "XMLHttpRequest";
      headers["GitHub-Verified-Fetch"] = "true";
      if (nonce) headers["X-Fetch-Nonce"] = nonce;
    }

    return ctx.util.request({
      method: "GET",
      url: url,
      headers: headers,
      timeoutMs: 15000,
    });
  }

  function extractFetchNonce(html) {
    const patterns = [
      /x-fetch-nonce"\s+content="([^"]+)"/i,
      /X-Fetch-Nonce"\s*:\s*"([^"]+)"/i,
      /fetchNonce"\s*:\s*"([^"]+)"/i,
      /data-fetch-nonce="([^"]+)"/i,
    ];
    for (let i = 0; i < patterns.length; i += 1) {
      const match = patterns[i].exec(html);
      if (match && match[1]) return match[1];
    }
    return null;
  }

  function fetchBudgetNonce(ctx, cookieHeader) {
    try {
      const resp = requestGitHubBudget(ctx, BUDGETS_URL, cookieHeader, "text/html,application/xhtml+xml", null);
      if (resp.status !== 200) return null;
      return extractFetchNonce(resp.bodyText || "");
    } catch (e) {
      ctx.host.log.info("Copilot budget nonce fetch failed: " + String(e));
      return null;
    }
  }

  function budgetPageUrl(page) {
    return BUDGETS_URL + "?page=" + encodeURIComponent(String(page)) + "&page_size=10&scope=customer";
  }

  function fetchBudgetPage(ctx, cookieHeader, nonce, page) {
    const resp = requestGitHubBudget(ctx, budgetPageUrl(page), cookieHeader, "application/json", nonce);
    if (resp.status === 401 || resp.status === 403) {
      throw "GitHub web session is not logged in.";
    }
    if (resp.status < 200 || resp.status >= 300) {
      throw "GitHub budget request failed (HTTP " + String(resp.status) + ").";
    }
    const json = ctx.util.tryParseJson(resp.bodyText);
    if (!json) throw "GitHub budget response invalid.";
    const payload = json.payload && typeof json.payload === "object" ? json.payload : json;
    let budgets = [];
    if (Array.isArray(payload.budgets)) budgets = payload.budgets;
    else if (Array.isArray(payload.data)) budgets = payload.data;
    else if (Array.isArray(json)) budgets = json;
    const hasNextPage = payload.hasNextPage === true || payload.has_next_page === true;
    return { budgets: budgets, hasNextPage: hasNextPage };
  }

  function parseAmount(value, centsKey) {
    if (typeof value === "number" && Number.isFinite(value)) return centsKey ? value / 100 : value;
    if (typeof value === "string" && value.trim()) {
      const trimmed = value.trim();
      const negative = trimmed[0] === "-";
      if (trimmed.slice(negative ? 1 : 0).indexOf("-") >= 0) return null;
      const unsigned = trimmed.replace(/[^0-9.]/g, "");
      if (!unsigned) return null;
      const parsed = Number((negative ? "-" : "") + unsigned);
      return Number.isFinite(parsed) ? parsed : null;
    }
    if (value && typeof value === "object") {
      const keys = ["amount", "value", "total", "cents", "formatted"];
      for (let i = 0; i < keys.length; i += 1) {
        const key = keys[i];
        const parsed = parseAmount(value[key], key === "cents");
        if (parsed !== null) return parsed;
      }
    }
    return null;
  }

  function readString(row, keys) {
    for (let i = 0; i < keys.length; i += 1) {
      const value = row && row[keys[i]];
      if (typeof value === "string" && value.trim()) return value.trim();
      if (typeof value === "number" && Number.isFinite(value)) return String(value);
    }
    return null;
  }

  function productSelectorsFromObject(value) {
    if (!value || typeof value !== "object") return [];
    const keys = ["sku", "name", "display_name", "displayName", "product", "product_name", "productName"];
    const out = [];
    for (let i = 0; i < keys.length; i += 1) {
      const text = readString(value, [keys[i]]);
      if (text) out.push(text);
    }
    return out;
  }

  function readStringArray(row, keys) {
    for (let i = 0; i < keys.length; i += 1) {
      const value = row && row[keys[i]];
      if (Array.isArray(value) && value.length) {
        const out = [];
        for (let j = 0; j < value.length; j += 1) {
          if (typeof value[j] === "string" && value[j].trim()) out.push(value[j].trim());
          else out.push.apply(out, productSelectorsFromObject(value[j]));
        }
        if (out.length) return out;
      }
      if (typeof value === "string" && value.trim()) return [value.trim()];
    }
    return [];
  }

  function readAmount(row, keys) {
    for (let i = 0; i < keys.length; i += 1) {
      const parsed = parseAmount(row && row[keys[i]], false);
      if (parsed !== null) return parsed;
    }
    return 0;
  }

  function slug(value) {
    return String(value || "")
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "");
  }

  function normalizedBillingIdentifier(value) {
    const raw = slug(value);
    if (!raw) return null;
    const underscored = raw.replace(/-/g, "_");
    if (underscored === COPILOT_PRODUCT_ID) return COPILOT_PRODUCT_ID;
    if (underscored === "premium_request" || underscored === "premium_requests") {
      return COPILOT_PREMIUM_REQUEST_SKU;
    }
    if (underscored === "coding_agent_premium_request" || underscored === "coding_agent_premium_requests") {
      return COPILOT_AGENT_PREMIUM_REQUEST_SKU;
    }
    if (underscored.indexOf("spark") >= 0 &&
        underscored.indexOf("premium") >= 0 &&
        underscored.indexOf("request") >= 0) {
      return SPARK_PREMIUM_REQUEST_SKU;
    }
    if ((underscored.indexOf("cloud") >= 0 || underscored.indexOf("coding") >= 0) &&
        underscored.indexOf("agent") >= 0 &&
        underscored.indexOf("premium") >= 0 &&
        underscored.indexOf("request") >= 0) {
      return COPILOT_AGENT_PREMIUM_REQUEST_SKU;
    }
    if (underscored.indexOf("bundled") >= 0 &&
        underscored.indexOf("premium") >= 0 &&
        underscored.indexOf("request") >= 0) {
      return COPILOT_PREMIUM_REQUEST_SKU;
    }
    if (underscored.indexOf("copilot") >= 0 &&
        underscored.indexOf("agent") >= 0 &&
        underscored.indexOf("premium") >= 0 &&
        underscored.indexOf("request") >= 0) {
      return COPILOT_AGENT_PREMIUM_REQUEST_SKU;
    }
    if (underscored.indexOf("copilot") >= 0 &&
        underscored.indexOf("premium") >= 0 &&
        underscored.indexOf("request") >= 0) {
      return COPILOT_PREMIUM_REQUEST_SKU;
    }
    return underscored;
  }

  function parseBudget(row) {
    const productSkus = readStringArray(row, [
      "budget_product_skus",
      "budgetProductSkus",
      "budget_product_sku",
      "budgetProductSku",
      "product_skus",
      "productSkus",
      "skus",
      "sku",
      "product",
      "product_name",
      "productName",
      "pricing_target_id",
      "pricingTargetId",
    ]);
    const budget = {
      id: readString(row, ["id", "uuid", "budget_id", "budgetId"]),
      name: readString(row, ["name", "display_name", "displayName", "title"]),
      budgetType: readString(row, ["budget_type", "budgetType", "type", "pricing_target_type", "pricingTargetType"]),
      budgetProductSkus: productSkus,
      budgetEntityName: readString(row, [
        "budget_entity_name",
        "budgetEntityName",
        "entity_name",
        "entityName",
        "target_name",
        "targetName",
      ]),
      budgetAmount: readAmount(row, [
        "budget_amount",
        "budgetAmount",
        "target_amount",
        "targetAmount",
        "spending_limit",
        "spendingLimit",
        "limit",
        "amount",
        "max",
      ]),
      currentAmount: readAmount(row, [
        "current_usage",
        "currentUsage",
        "current_amount",
        "currentAmount",
        "usage_amount",
        "usageAmount",
        "usage",
        "spent",
        "amount_used",
        "amountUsed",
      ]),
    };
    const selectorValues = budget.budgetProductSkus.concat([
      budget.budgetType,
      budget.budgetEntityName,
      budget.name,
    ]).filter(Boolean);
    budget.selectors = selectorValues
      .map(normalizedBillingIdentifier)
      .filter(Boolean);
    return budget;
  }

  function hasCopilotSelector(selectors) {
    for (let i = 0; i < selectors.length; i += 1) {
      for (let j = 0; j < COPILOT_BUDGET_SELECTORS.length; j += 1) {
        if (selectors[i] === COPILOT_BUDGET_SELECTORS[j]) return true;
      }
    }
    return false;
  }

  function budgetTitle(budget) {
    const selectors = budget.selectors || [];
    let type = null;
    if (selectors.length === 1 && selectors[0] === COPILOT_PRODUCT_ID) {
      type = "Copilot";
    } else if (selectors.indexOf(COPILOT_AGENT_PREMIUM_REQUEST_SKU) >= 0) {
      type = "Copilot Agent Premium Requests";
    } else if (selectors.indexOf(SPARK_PREMIUM_REQUEST_SKU) >= 0) {
      type = "Spark Premium Requests";
    } else if (selectors.indexOf(COPILOT_PREMIUM_REQUEST_SKU) >= 0) {
      type = "All Premium Request SKUs";
    } else {
      type = budget.name || "Copilot Premium Requests";
    }
    return "Budget - " + type;
  }

  function nextMonthResetIso() {
    const now = new Date();
    return new Date(now.getFullYear(), now.getMonth() + 1, 1).toISOString();
  }

  function budgetToLine(ctx, budget, resetIso) {
    if (!budget || !(budget.budgetAmount > 0) || !hasCopilotSelector(budget.selectors || [])) return null;
    const used = Math.max(0, budget.currentAmount || 0);
    const limit = Math.max(0, budget.budgetAmount || 0);
    return ctx.line.progress({
      label: budgetTitle(budget),
      used: used,
      limit: limit,
      format: { kind: "dollars" },
      resetsAt: resetIso,
      detail: "$" + used.toFixed(2) + " / $" + limit.toFixed(2),
    });
  }

  function fetchBudgetLines(ctx) {
    const cookieHeader = loadBudgetCookieHeader(ctx);
    if (!cookieHeader) return [];

    const nonce = fetchBudgetNonce(ctx, cookieHeader);
    const budgets = [];
    let page = 1;
    let keepGoing = true;
    while (keepGoing && page <= 20) {
      const response = fetchBudgetPage(ctx, cookieHeader, nonce, page);
      for (let i = 0; i < response.budgets.length; i += 1) {
        budgets.push(response.budgets[i]);
      }
      keepGoing = response.hasNextPage === true;
      page += 1;
    }

    const resetIso = nextMonthResetIso();
    const lines = [];
    for (let i = 0; i < budgets.length; i += 1) {
      const line = budgetToLine(ctx, parseBudget(budgets[i]), resetIso);
      if (line) lines.push(line);
    }
    return lines;
  }

  function makeProgressLine(ctx, label, snapshot, resetDate) {
    if (!snapshot || snapshot.unlimited === true || snapshot.entitlement === -1 || snapshot.remaining === -1 || snapshot.entitlement === 0) return null;
    let percentRemaining = snapshot.percent_remaining;
    if (!Number.isFinite(percentRemaining) && Number.isFinite(snapshot.entitlement) && snapshot.entitlement > 0 && Number.isFinite(snapshot.remaining)) percentRemaining = snapshot.remaining / snapshot.entitlement * 100;
    if (!Number.isFinite(percentRemaining)) return null;
    const usedPercent = Math.min(100, Math.max(0, 100 - percentRemaining));
    return ctx.line.progress({
      label: label,
      used: usedPercent,
      limit: 100,
      format: { kind: "percent" },
      resetsAt: ctx.util.toIso(resetDate),
      periodDurationMs: calendarMonthDuration(ctx, resetDate),
    });
  }

  function makeLimitedProgressLine(ctx, label, remaining, total, resetDate) {
    if (typeof remaining !== "number" || typeof total !== "number" || total <= 0)
      return null;
    const used = total - remaining;
    const usedPercent = Math.min(100, Math.max(0, Math.round((used / total) * 100)));
    return ctx.line.progress({
      label: label,
      used: usedPercent,
      limit: 100,
      format: { kind: "percent" },
      resetsAt: ctx.util.toIso(resetDate),
      periodDurationMs: calendarMonthDuration(ctx, resetDate),
    });
  }

  function calendarMonthDuration(ctx, resetDate) {
    const end = ctx.util.parseDateMs(resetDate);
    if (!Number.isFinite(end)) return undefined;
    const start = new Date(end);
    start.setUTCMonth(start.getUTCMonth() - 1);
    return end - start.getTime();
  }

  function fetchOrgBillingLines(ctx, token) {
    const deadline = Date.now() + 3000;
    function get(path) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) return null;
      const result = ctx.util.requestJson({ method: "GET", url: "https://api.github.com" + path,
        headers: { Authorization: "token " + token, Accept: "application/vnd.github+json", "User-Agent": "usagestat", "X-GitHub-Api-Version": "2022-11-28" }, timeoutMs: remaining });
      return result.resp.status === 200 ? result.json : null;
    }
    const selected = ctx.provider && ctx.provider.workspaceId;
    const orgs = selected ? [{ login: selected }] : get("/user/orgs?per_page=100");
    if (!Array.isArray(orgs)) return [];
    const result = [];
    for (const org of orgs.slice(0, 5)) {
      if (!org || typeof org.login !== "string" || !/^[a-zA-Z0-9][a-zA-Z0-9-]*$/.test(org.login)) continue;
      const body = get("/orgs/" + encodeURIComponent(org.login) + "/settings/billing/usage/summary");
      if (!body || !Array.isArray(body.usageItems)) continue;
      const items = body.usageItems.filter((item) => item && String(item.product).toLowerCase() === "copilot" && ["ai-units", "ai-credits"].includes(String(item.unitType).toLowerCase()));
      if (!items.length || items.some((item) => !Number.isFinite(item.grossQuantity) || item.grossQuantity < 0 || !Number.isFinite(item.netAmount) || item.netAmount < 0)) continue;
      result.push(ctx.line.text({ label: "Org Credits", value: String(items.reduce((n, item) => n + item.grossQuantity, 0)) + " credits", subtitle: org.login + " - organization total" }));
      result.push(ctx.line.text({ label: "Org Spend", value: "$" + items.reduce((n, item) => n + item.netAmount, 0).toFixed(2), subtitle: org.login + " - this month" }));
    }
    return result;
  }

  function probe(ctx) {
    const cred = loadToken(ctx);
    if (!cred) {
      throw "Not logged in. Run `gh auth login` first.";
    }

    let token = cred.token;
    let source = cred.source;

    let resp;
    try {
      resp = fetchUsage(ctx, token);
    } catch (e) {
      ctx.host.log.error("usage request exception: " + String(e));
      throw "Usage request failed. Check your connection.";
    }

    if (resp.status === 401 || resp.status === 403) {
      // If cached token is stale, clear it and try fallback sources
      if (source === "keychain") {
        ctx.host.log.info("cached token invalid, trying fallback sources");
        clearCachedToken(ctx);
        const fallback = loadTokenFromGhCli(ctx);
        if (fallback) {
          try {
            resp = fetchUsage(ctx, fallback.token);
          } catch (e) {
            ctx.host.log.error("fallback usage request exception: " + String(e));
            throw "Usage request failed. Check your connection.";
          }
          if (resp.status >= 200 && resp.status < 300) {
            // Fallback worked, persist the new token
            saveToken(ctx, fallback.token);
            token = fallback.token;
            source = fallback.source;
          }
        }
      }
      // Still failing after retry
      if (resp.status === 401 || resp.status === 403) {
        throw "Token invalid. Run `gh auth login` to re-authenticate.";
      }
    }

    if (resp.status < 200 || resp.status >= 300) {
      ctx.host.log.error("usage returned error: status=" + resp.status);
      throw (
        "Usage request failed (HTTP " +
        String(resp.status) +
        "). Try again later."
      );
    }

    // Persist gh-cli token to OpenUsage keychain for future use
    if (source === "gh-cli" || source === "gh-command") {
      saveToken(ctx, token);
    }

    const data = ctx.util.tryParseJson(resp.bodyText);
    if (data === null) {
      throw "Usage response invalid. Try again later.";
    }

    ctx.host.log.info("usage fetch succeeded");

    const lines = [];
    let plan = null;
    if (data.copilot_plan) {
      plan = ctx.fmt.planLabel(data.copilot_plan);
    }

    // Paid tier: quota_snapshots
    const snapshots = data.quota_snapshots;
    if (snapshots) {
      const premiumLine = makeProgressLine(
        ctx,
        "Credits",
        snapshots.premium_interactions,
        data.quota_reset_date,
      );
      if (premiumLine) {
        lines.push(premiumLine);
        const premium = snapshots.premium_interactions;
        if (premium.overage_permitted === true && Number.isFinite(premium.overage_count) && premium.overage_count >= 0) lines.push(ctx.line.text({ label: "Extra Usage", value: String(premium.overage_count) }));
      }

      const chatLine = makeProgressLine(
        ctx,
        "Chat",
        snapshots.chat,
        data.quota_reset_date,
      );
      if (chatLine) lines.push(chatLine);
      const completionsLine = makeProgressLine(ctx, "Completions", snapshots.completions, data.quota_reset_date);
      if (completionsLine) lines.push(completionsLine);
    }

    // Free tier: limited_user_quotas
    if (!lines.length && data.limited_user_quotas && data.monthly_quotas) {
      const lq = data.limited_user_quotas;
      const mq = data.monthly_quotas;
      const resetDate = data.limited_user_reset_date;

      const chatLine = makeLimitedProgressLine(ctx, "Chat", lq.chat, mq.chat, resetDate);
      if (chatLine) lines.push(chatLine);

      const completionsLine = makeLimitedProgressLine(ctx, "Completions", lq.completions, mq.completions, resetDate);
      if (completionsLine) lines.push(completionsLine);
    }

    const orgManaged = !lines.length && data.token_based_billing === true;
    if (orgManaged) {
      const credits = snapshots && snapshots.premium_interactions && snapshots.premium_interactions.credits_used;
      if (Number.isFinite(credits) && credits > 0) lines.push(ctx.line.text({ label: "Credits", value: String(credits) + " credits" }));
      try {
        lines.push(...fetchOrgBillingLines(ctx, token));
      } catch (_) {
        ctx.host.log.warn("Copilot organization billing unavailable for this token");
      }
    }

    try {
      const budgetLines = fetchBudgetLines(ctx);
      for (let i = 0; i < budgetLines.length; i += 1) {
        lines.push(budgetLines[i]);
      }
    } catch (e) {
      ctx.host.log.warn("Copilot budget extras failed: " + String(e));
    }

    if (lines.length === 0) {
      lines.push(
        ctx.line.badge({
          label: "Status",
          text: "No usage data",
          color: "#a3a3a3",
        }),
      );
    }

    return { plan: plan, lines: lines, source: "api" };
  }

  globalThis.__openusage_plugin = { id: "copilot", probe };
})();
