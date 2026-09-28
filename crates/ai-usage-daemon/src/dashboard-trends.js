// Daily reports are additive; polling snapshots are not. Only pass selected
// daily rows from /v1/history/daily to this module (one row per provider/day).
const UsageTrends = (() => {
  const DAY = 86400000;
  const fields = ['inputTokens', 'outputTokens', 'cacheReadTokens', 'cacheCreationTokens',
    'reasoningOutputTokens', 'totalTokens', 'cost'];
  const costFields = ['inputCostUsd', 'cacheReadCostUsd', 'cacheWriteCostUsd', 'outputCostUsd'];
  const key = ms => new Date(ms).toISOString().slice(0, 10);
  function dateMs(value) {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(value || '')) return NaN;
    const ms = Date.parse(value + 'T00:00:00Z');
    return Number.isFinite(ms) && key(ms) === value ? ms : NaN;
  }
  function range(rows, options = {}, today = key(Date.now())) {
    const end = dateMs(today);
    let start = end - 29 * DAY;
    if (options.range === 'custom') {
      const a = dateMs(options.start), b = dateMs(options.end);
      if (!Number.isFinite(a) || !Number.isFinite(b)) return { error: 'Choose a valid start and end date.' };
      if (a > b) return { error: 'Start date must be on or before end date.' };
      if (b > end) return { error: 'End date cannot be in the future.' };
      return makeRange(a, b);
    }
    if (options.range === 'month') {
      start = dateMs(today.slice(0, 7) + '-01');
    } else if (options.range === 'all') {
      const dates = rows.map(r => dateMs(r.date)).filter(ms => Number.isFinite(ms) && ms <= end);
      if (dates.length) start = Math.min(...dates);
    } else if (['7', '30', '90', '365'].includes(String(options.range))) {
      start = end - (Number(options.range) - 1) * DAY;
    }
    return makeRange(start, end);
  }
  function makeRange(start, end) {
    const days = Math.round((end - start) / DAY) + 1;
    // Bound accidental imports/custom dates before allocating calendar buckets.
    if (days > 36600) return { error: 'Choose a range of at most 100 years.' };
    return { start: key(start), end: key(end), days,
      previousStart: key(start - days * DAY), previousEnd: key(start - DAY) };
  }
  function select(rows, start, end) {
    return rows.filter(r => Number.isFinite(dateMs(r.date)) && r.date >= start && r.date <= end);
  }
  function empty() { return {...Object.fromEntries([...fields, ...costFields].map(f => [f, 0])), costComponentsKnown:true, costKnown: true, tokensKnown: true, sessions:0, sessionsKnown:true, cacheSavingsUsd:0, cacheSavingsKnown:true}; }
  function add(total, row) {
    total.costComponentsKnown &&= row.costKnown !== false && costFields.every(f => Number.isFinite(row[f]) && row[f]>=0)
      && Math.abs(costFields.reduce((sum,f)=>sum+row[f],0)-row.cost)<=Math.max(1,Math.abs(row.cost))*1e-9;
    for(const f of costFields) total[f] = total.costComponentsKnown ? total[f]+row[f] : null;
    total.sessionsKnown &&= Number.isSafeInteger(row.sessions) && row.sessions>=0;
    total.cacheSavingsKnown &&= Number.isFinite(row.cacheSavingsUsd) && row.cacheSavingsUsd>=0;
    total.sessions += Number.isSafeInteger(row.sessions) && row.sessions>=0 ? row.sessions : 0;
    total.cacheSavingsUsd += Number.isFinite(row.cacheSavingsUsd) && row.cacheSavingsUsd>=0 ? row.cacheSavingsUsd : 0;
    total.costKnown &&= row.costKnown !== false;
    total.tokensKnown &&= row.tokensKnown !== false;
    for (const f of fields) {
      const n = Number(row[f]);
      if (Number.isFinite(n) && n >= 0) total[f] += n;
    }
  }
  function totals(rows) {
    const out = empty(), days = new Set(), active = new Set(), providers = new Set();
    for (const r of rows) {
      add(out, r);
      days.add(r.date);
      providers.add(r.providerId.toLowerCase());
      if (r.totalTokens > 0 || r.cost > 0) active.add(r.date);
    }
    return { ...out, recordedDays: days.size, activeDays: active.size, providers: providers.size, rows: rows.length };
  }
  function change(current, previous, currentRows, previousRows) {
    if (!currentRows || !previousRows) return { kind: 'unavailable', percent: null };
    if (previous === 0) return { kind: current === 0 ? 'flat' : 'fromZero', percent: current === 0 ? 0 : null };
    return { kind: 'percent', percent: (current - previous) / previous * 100 };
  }
  function bucketStart(date, group) {
    const ms = dateMs(date), d = new Date(ms);
    if (group === 'month') return date.slice(0, 7) + '-01';
    if (group === 'week') return key(ms - ((d.getUTCDay() + 6) % 7) * DAY);
    return date;
  }
  function buckets(rows, selectedRange, group) {
    const byDay = new Map();
    for (const r of rows) {
      if (!byDay.has(r.date)) byDay.set(r.date, []);
      byDay.get(r.date).push(r);
    }
    const map = new Map();
    for (let ms = dateMs(selectedRange.start); ms <= dateMs(selectedRange.end); ms += DAY) {
      const date = key(ms), bucket = bucketStart(date, group);
      if (!map.has(bucket)) map.set(bucket, { ...empty(), start: date, end: date, days: 0, recordedDays: 0, rows: 0 });
      const b = map.get(bucket), daily = byDay.get(date) || [];
      b.end = date;
      b.days++;
      b.rows += daily.length;
      if (daily.length) b.recordedDays++;
      for (const r of daily) add(b, r);
    }
    return [...map.values()];
  }
  function summarize(rows, options = {}, today) {
    const provider = String(options.provider || '').toLowerCase();
    const selected = rows.filter(r => !provider || r.providerId.toLowerCase() === provider);
    const period = range(selected, options, today);
    if (period.error) return period;
    const currentRows = select(selected, period.start, period.end);
    const previousRows = select(selected, period.previousStart, period.previousEnd);
    const current = totals(currentRows), previous = totals(previousRows);
    const group = ['day', 'week', 'month'].includes(options.group) ? options.group : 'day';
    const ids = new Map();
    for (const r of [...previousRows, ...currentRows]) ids.set(r.providerId.toLowerCase(), r.displayName || r.providerId);
    const providers = [...ids].map(([id, displayName]) => ({ id, displayName,
      current: totals(currentRows.filter(r => r.providerId.toLowerCase() === id)),
      previous: totals(previousRows.filter(r => r.providerId.toLowerCase() === id))
    })).sort((a, b) => b.current.totalTokens - a.current.totalTokens || a.displayName.localeCompare(b.displayName));
    return { range: period, current, previous, rows: currentRows, providers,
      buckets: buckets(currentRows, period, group) };
  }
  return { dateMs, range, totals, change, summarize };
})();

if (typeof module !== 'undefined' && module.exports) module.exports = UsageTrends;
