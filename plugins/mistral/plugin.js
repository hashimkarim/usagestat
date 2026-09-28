(function () {
  var BASE_URL = "https://admin.mistral.ai"
  var USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/124.0.0.0 Safari/537.36"

  function getCookieHeader(ctx) {
    var provider = ctx.provider || {}
    var raw = Object.prototype.hasOwnProperty.call(provider, 'cookieHeader') ? provider.cookieHeader :
      provider.instanceId && provider.instanceId !== 'mistral' ? null : ctx.host.env.get("MISTRAL_COOKIE")
    if (!raw || !raw.trim()) {
      throw "Set MISTRAL_COOKIE to your cookie header from admin.mistral.ai. Include csrftoken if present."
    }
    return raw.trim()
  }

  function extractCsrf(cookieHeader) {
    var parts = cookieHeader.split(";")
    for (var i = 0; i < parts.length; i++) {
      var pair = parts[i].trim()
      var eq = pair.indexOf("=")
      if (eq < 0) continue
      var name = pair.slice(0, eq).trim()
      if (name === "csrftoken") return pair.slice(eq + 1).trim()
    }
    return null
  }

  function currentMonthYear() {
    var now = new Date()
    return { month: now.getUTCMonth() + 1, year: now.getUTCFullYear() }
  }

  function aggregateModels(models, prices) {
    var total = 0
    var inputTokens = 0
    var outputTokens = 0
    if (!models) return { cost: 0, inputTokens: 0, outputTokens: 0 }
    var keys = Object.keys(models)
    for (var i = 0; i < keys.length; i++) {
      var data = models[keys[i]]
      total += sumEntries(data.input, prices)
      total += sumEntries(data.output, prices)
      total += sumEntries(data.cached, prices)
      inputTokens += countEntries(data.input)
      outputTokens += countEntries(data.output)
    }
    return { cost: total, inputTokens: inputTokens, outputTokens: outputTokens }
  }

  function sumEntries(entries, prices) {
    if (!entries) return 0
    var total = 0
    for (var i = 0; i < entries.length; i++) {
      var e = entries[i]
      var paid = e.value_paid != null ? e.value_paid : (e.value || 0)
      var key = e.billing_metric + "::" + e.billing_group
      var price = prices[key] || 0
      total += paid * price
    }
    return total
  }

  function countEntries(entries) {
    if (!entries) return 0
    var total = 0
    for (var i = 0; i < entries.length; i++) {
      var e = entries[i]
      total += Number(e.value != null ? e.value : (e.value_paid || 0))
    }
    if (!Number.isSafeInteger(total) || total < 0) throw new Error('Invalid Mistral token total.')
    return total
  }

  function buildPriceIndex(prices) {
    var index = {}
    if (!prices) return index
    for (var i = 0; i < prices.length; i++) {
      var p = prices[i]
      if (p.billing_metric && p.billing_group && p.price) {
        var key = p.billing_metric + "::" + p.billing_group
        index[key] = parseFloat(p.price) || 0
      }
    }
    return index
  }

  function subscriptionBudgets(html) {
    if (typeof html !== 'string' || html.length > 2 * 1024 * 1024) throw new Error('Invalid subscription page.')
    var chunks = [], marker = 'self.__next_f.push(', cursor = 0
    while ((cursor = html.indexOf(marker, cursor)) >= 0) {
      cursor += marker.length
      var start = cursor, stack = [], quoted = false, escaped = false
      while (/\s/.test(html[start] || '') && start < html.length) start++
      if (html[start] !== '[') continue
      for (var end = start; end < html.length; end++) {
        var ch = html[end]
        if (quoted) { if (escaped) escaped = false; else if (ch === '\\') escaped = true; else if (ch === '"') quoted = false }
        else if (ch === '"') quoted = true
        else if (ch === '[' || ch === '{') stack.push(ch === '[' ? ']' : '}')
        else if (ch === ']' || ch === '}') {
          if (stack.pop() !== ch) throw new Error('Invalid subscription record.')
          if (!stack.length) {
            var value = JSON.parse(html.slice(start,end+1))
            if (value[0] === 1 && typeof value[1] === 'string') chunks.push(value[1])
            cursor = end+1; break
          }
        }
      }
    }
    // Flight's text/binary records are byte-counted and may contain fake JSON rows.
    var stream = encodeURIComponent(chunks.join('')).replace(/%([0-9A-F]{2})/g, function(_,hex){return String.fromCharCode(parseInt(hex,16))})
    var found = new Map()
    function budget(raw) {
      if (!raw || typeof raw.usage_percentage !== 'number' || !Number.isFinite(raw.usage_percentage) || raw.usage_percentage < 0 ||
          typeof raw.initial_budget !== 'number' || !Number.isFinite(raw.initial_budget) || raw.initial_budget <= 0 ||
          !/^[A-Za-z]{3}$/.test(raw.currency || '')) return null
      return {percent:raw.usage_percentage,limit:raw.initial_budget,currency:raw.currency.toUpperCase(),reset:raw.reset_at || null}
    }
    function collect(value, depth) {
      if (depth > 64) throw new Error('Subscription record too deep.')
      if (!value || typeof value !== 'object') return
      if (value.budget) {
        var pair = [budget(value.budget.api_budget),budget(value.budget.vibe_budget)]
        if (pair.some(Boolean)) found.set(JSON.stringify(pair),pair)
      }
      for (var child of Object.values(value)) collect(child,depth+1)
    }
    cursor = 0
    while (cursor < stream.length) {
      var newline = stream.indexOf('\n',cursor); if (newline < 0) newline = stream.length
      var row = stream.slice(cursor,newline), match = /^[0-9a-f]+:/i.exec(row)
      if (!match) { cursor = newline+1; continue }
      var body = row.slice(match[0].length)
      if (/^[TAOoUSsLlGgMmV]/.test(body)) {
        var length = /^[TAOoUSsLlGgMmV]([0-9a-f]+),/i.exec(body)
        if (!length) throw new Error('Invalid Flight length.')
        cursor += match[0].length+length[0].length+parseInt(length[1],16)
        if (cursor > stream.length) throw new Error('Incomplete Flight record.')
        continue
      }
      if (/^[\[{]/.test(body)) {
        var text = decodeURIComponent(Array.from(body,function(c){return '%'+c.charCodeAt(0).toString(16).padStart(2,'0')}).join(''))
        collect(JSON.parse(text),0)
      }
      cursor = newline+1
    }
    if (found.size !== 1) throw new Error('Subscription budgets absent or ambiguous.')
    return found.values().next().value
  }

  function optionalAllowances(ctx, headers, cookie, csrf, lines) {
    var hasVibe = false
    try {
      var page = ctx.host.http.request({method:'GET',url:BASE_URL+'/subscription',headers:headers,timeoutMs:3000})
      if (page.status === 200) subscriptionBudgets(page.bodyText).forEach(function(b,index){
        if (!b) return
        var used = b.limit*b.percent/100
        if (!Number.isFinite(used)) return
        var reset = ctx.util.toIso(b.reset)
        lines.push(ctx.line.progress({label:index?'Monthly Plan':'Included API',used:b.percent,limit:100,
          format:{kind:'percent'},resetsAt:reset || undefined,
          detail:b.currency+' '+used.toFixed(2)+' / '+b.limit.toFixed(2)}))
        if (index) hasVibe = true
      })
    } catch (_) {}
    if (!hasVibe && csrf) try {
      var minimal = cookie.split(';').map(function(p){return p.trim()}).filter(function(p){return /^(csrftoken|ory_session_[^=]+)=/.test(p)}).join('; ')
      var url = 'https://console.mistral.ai/api-ui/trpc/billing.vibeUsage?batch=1&input='+encodeURIComponent(JSON.stringify({'0':{json:null,meta:{values:['undefined'],v:1}}}))
      var resp = ctx.host.http.request({method:'GET',url:url,headers:{Cookie:minimal,'X-CSRFTOKEN':csrf,Accept:'application/json'},timeoutMs:2000})
      var vibe = resp.status===200 ? ctx.util.tryParseJson(resp.bodyText)?.[0]?.result?.data?.json : null
      if (typeof vibe?.usage_percentage==='number' && vibe.usage_percentage>=0 && vibe.usage_percentage<=100)
        lines.push(ctx.line.progress({label:'Monthly Plan',used:vibe.usage_percentage,limit:100,format:{kind:'percent'},resetsAt:ctx.util.toIso(vibe.reset_at) || undefined}))
    } catch (_) {}
    try {
      var credits = ctx.util.requestJson({method:'GET',url:BASE_URL+'/api/billing/credits',headers:headers,timeoutMs:2000})
      var data = credits.json
      if (credits.resp.status!==200 || typeof data?.wallet_amount!=='number' || !/^[A-Za-z]{3}$/.test(data.currency || '')) return
      var values = [data.wallet_amount,data.credit_notes_amount ?? 0,data.ongoing_usage_balance ?? 0]
      if (!values.every(function(n){return typeof n==='number'&&Number.isFinite(n)})) return
      var amount = values[0]+values[1]-values[2]
      if (Number.isFinite(amount)) lines.push(ctx.line.text({label:'Credit balance',value:data.currency.toUpperCase()+' '+amount.toFixed(2)}))
    } catch (_) {}
  }

  function probe(ctx) {
    var cookieHeader = getCookieHeader(ctx)
    var csrf = extractCsrf(cookieHeader)
    var my = currentMonthYear()

    var headers = {
      "Cookie": cookieHeader,
      "Accept": "*/*",
      "Origin": BASE_URL,
      "Referer": BASE_URL + "/organization/usage",
      "User-Agent": USER_AGENT,
    }
    if (csrf) headers["X-CSRFTOKEN"] = csrf

    var result = ctx.util.requestJson({
      method: "GET",
      url: BASE_URL + "/api/billing/v2/usage?month=" + my.month + "&year=" + my.year,
      headers: headers,
      timeoutMs: 20000,
    })

    if (result.resp.status === 401 || result.resp.status === 403) {
      throw "Session expired. Update MISTRAL_COOKIE with fresh cookies from admin.mistral.ai."
    }
    if (result.resp.status < 200 || result.resp.status >= 300) {
      throw "Mistral API returned HTTP " + result.resp.status + "."
    }

    var billing = result.json
    if (!billing) throw "Could not parse Mistral billing response."

    var prices = buildPriceIndex(billing.prices)
    var currency = billing.currency || "EUR"
    var symbol = billing.currency_symbol || "€"

    var totalCost = 0
    var totalInput = 0
    var totalOutput = 0

    var completions = [billing.completion, billing.chat, billing.vibe_code && billing.vibe_code.completion]
    for (var c = 0; c < completions.length; c++) {
      if (!completions[c] || !completions[c].models) continue
      var r = aggregateModels(completions[c].models, prices)
      totalCost += r.cost; totalInput += r.inputTokens; totalOutput += r.outputTokens
    }

    var extras = [billing.ocr, billing.connectors, billing.audio]
    for (var i = 0; i < extras.length; i++) {
      if (extras[i] && extras[i].models) {
        totalCost += aggregateModels(extras[i].models, prices).cost
      }
    }

    if (billing.libraries_api) {
      var lib = billing.libraries_api
      if (lib.pages && lib.pages.models) totalCost += aggregateModels(lib.pages.models, prices).cost
      if (lib.tokens && lib.tokens.models) totalCost += aggregateModels(lib.tokens.models, prices).cost
    }

    var costStr = symbol + totalCost.toFixed(4) + " this month (" + currency + ")"
    var tokenDetail = totalInput + " in / " + totalOutput + " out tokens"

    var lines = [
      ctx.line.text({ label: "Monthly spend", value: costStr }),
      ctx.line.text({ label: "Tokens", value: tokenDetail }),
    ]

    optionalAllowances(ctx,headers,cookieHeader,csrf,lines)

    return { source:'web', lines: lines }
  }

  globalThis.__openusage_plugin = { id: "mistral", probe: probe }
})()
