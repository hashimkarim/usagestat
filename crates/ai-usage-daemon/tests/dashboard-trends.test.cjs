const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const trends = require('../src/dashboard-trends.js');

const row = (date, totalTokens, cost = 0, providerId = 'codex') => ({
  date, providerId, displayName: providerId, totalTokens, cost,
  inputTokens: totalTokens, outputTokens: 0, cacheReadTokens: 0,
  cacheCreationTokens: 0, reasoningOutputTokens: 0,
});
const today = '2026-09-06';

test('calendar ranges include both endpoints and compare equal preceding periods', () => {
  const result = trends.summarize([
    row('2026-08-23', 999), row('2026-08-24', 10), row('2026-08-30', 20),
    row('2026-08-31', 40), row(today, 60), row('2026-09-07', 999),
  ], { range: '7' }, today);
  assert.deepEqual(result.range, {
    start: '2026-08-31', end: today, days: 7,
    previousStart: '2026-08-24', previousEnd: '2026-08-30',
  });
  assert.equal(result.current.totalTokens, 100);
  assert.equal(result.previous.totalTokens, 30);
  assert.equal(result.current.recordedDays, 2);
});

test('all-provider totals sum each saved row, while provider filters ignore case', () => {
  const rows = [row(today, 100, 1), row(today, 200, 2, 'Claude'), row('2026-09-05', 50, .5)];
  const all = trends.summarize(rows, {}, today);
  assert.equal(all.current.totalTokens, 350);
  assert.equal(all.current.cost, 3.5);
  assert.equal(all.current.recordedDays, 2);
  assert.equal(all.current.providers, 2);
  const claude = trends.summarize(rows, { provider: 'CLAUDE' }, today);
  assert.equal(claude.current.totalTokens, 200);
  assert.equal(claude.rows.length, 1);
  assert.equal(claude.providers.length, 1);
});

test('recorded zero and missing days remain distinct in totals and buckets', () => {
  const result = trends.summarize([row(today, 0)], { range: '7' }, today);
  assert.equal(result.current.recordedDays, 1);
  assert.equal(result.current.activeDays, 0);
  assert.equal(result.buckets.length, 7);
  assert.equal(result.buckets[0].rows, 0);
  assert.equal(result.buckets[6].rows, 1);
  assert.equal(result.buckets[6].totalTokens, 0);
  assert.deepEqual(trends.change(10, 0, 1, 0), { kind: 'unavailable', percent: null });
  assert.deepEqual(trends.change(10, 0, 1, 1), { kind: 'fromZero', percent: null });
  assert.equal(trends.change(0, 10, 0, 1).kind, 'unavailable');
  assert.equal(trends.change(0, 10, 1, 1).percent, -100);
  assert.equal(trends.change(15, 10, 1, 1).percent, 50);
  assert.equal(trends.change(0, 0, 1, 1).percent, 0);
});

test('custom weeks start on Monday and clip edge buckets to the selected dates', () => {
  const result = trends.summarize([
    row('2026-08-30', 999), row('2026-09-02', 100), row('2026-09-06', 200), row('2026-09-07', 50),
  ], { range: 'custom', start: '2026-09-02', end: '2026-09-08', group: 'week' }, '2026-09-10');
  assert.equal(result.buckets.length, 2);
  assert.deepEqual(result.buckets.map(b => [b.start, b.end, b.totalTokens, b.days, b.recordedDays]), [
    ['2026-09-02', '2026-09-06', 300, 5, 2], ['2026-09-07', '2026-09-08', 50, 2, 1],
  ]);
});

test('month buckets cross leap days and year boundaries in UTC', () => {
  const result = trends.summarize([row('2024-02-29', 9), row('2024-03-01', 11)],
    { range: 'custom', start: '2024-02-28', end: '2024-03-01', group: 'month' }, today);
  assert.deepEqual(result.buckets.map(b => [b.days, b.totalTokens]), [[2, 9], [1, 11]]);
  const year = trends.range([], { range: '7' }, '2026-01-03');
  assert.equal(year.start, '2025-12-28');
  assert.equal(year.previousStart, '2025-12-21');
});

test('all-history range follows the selected provider and excludes future/invalid rows', () => {
  const rows = [row('2020-01-01', 1, 0, 'old'), row('2026-09-01', 10), row('2026-02-30', 999), row('2027-01-01', 999)];
  const result = trends.summarize(rows, { provider: 'codex', range: 'all' }, today);
  assert.equal(result.range.start, '2026-09-01');
  assert.equal(result.range.end, today);
  assert.equal(result.current.totalTokens, 10);
  assert.equal(trends.summarize([], { range: 'all' }, today).buckets.length, 30);
});

test('invalid custom dates fail explicitly without allocating huge calendars', () => {
  for (const [start, end] of [['2026-02-30', today], [today, '2026-01-01'], ['', today],
    [today, '2027-01-01'], ['0001-01-01', today]]) {
    assert.ok(trends.summarize([], { range: 'custom', start, end }, today).error);
  }
});

test('missing providers in the current period retain their previous totals', () => {
  const result = trends.summarize([row('2026-08-25', 5, 2, 'retired')], { range: '7' }, today);
  assert.equal(result.current.rows, 0);
  assert.equal(result.providers[0].previous.cost, 2);
  assert.equal(result.providers[0].current.rows, 0);
});

test('all token components survive grouping and invalid numbers cannot poison totals', () => {
  const result = trends.summarize([{ ...row(today, 10, 1), inputTokens: 1, outputTokens: 2,
    cacheReadTokens: 3, cacheCreationTokens: 1, reasoningOutputTokens: 3 },
    { ...row(today, NaN, Infinity, 'bad'), inputTokens: -1 }], { group: 'month' }, today);
  assert.equal(result.current.totalTokens, 10);
  assert.equal(result.current.cost, 1);
  assert.equal(result.current.reasoningOutputTokens, 3);
  assert.equal(result.buckets.at(-1).cacheReadTokens, 3);
});

// Compile the actual embedded UI too: syntax errors here otherwise hide all tabs.
test('dashboard scripts compile and the browser trends asset is referenced', () => {
  const html = fs.readFileSync(require.resolve('../src/dashboard.html'), 'utf8');
  assert.match(html, /src="\/dashboard\/trends\.js"/);
  for (const script of html.matchAll(/<script>([\s\S]*?)<\/script>/g)) new vm.Script(script[1]);
});

function dashboard(rows = [], prefs = {}) {
  const html = fs.readFileSync(require.resolve('../src/dashboard.html'), 'utf8');
  const script = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)][0][1].replace(/load\(\);\s*$/, '');
  const elements = new Map();
  const element = id => {
    if (!elements.has(id)) elements.set(id, { style: {}, innerHTML: '',
      querySelector: selector => element(selector),
      addEventListener: (event, fn) => { element(id)[event] = fn; } });
    return elements.get(id);
  };
  const context = vm.createContext({ UsageTrends: trends, fixture: rows,
    setInterval: () => 0, localStorage: { getItem: () => JSON.stringify({ trends: prefs }), setItem: () => {} },
    document: { getElementById: element } });
  vm.runInContext(script, context);
  vm.runInContext("S.dailyRows = fixture; todayUtcDate = () => new Date('2026-09-06T00:00:00Z');", context);
  // Inject the clock into the pure model, whose default otherwise uses real time.
  context.UsageTrends = { ...trends, summarize: (rows, options) => trends.summarize(rows, options, today) };
  return { context, element, run: code => vm.runInContext(code, context) };
}

test('History renders saved providers, distinct gaps, comparisons, and safe labels', () => {
  const ui = dashboard([row(today, 10, 2, '<retired>'), row('2026-08-25', 5, 1, '<retired>')], { range: '7' });
  ui.run('renderTrends()');
  const html = ui.element('panel-history').innerHTML;
  assert.match(html, /&lt;retired&gt;/);
  assert.doesNotMatch(html, /<retired>/);
  assert.match(html, /\+100\.0% vs previous period/);
  assert.match(html, /1\/7/);
  assert.match(html, /No record/);
  assert.match(html, /Today is partial/);
});

test('History distinguishes an API failure from a successful empty response', () => {
  const ui = dashboard();
  ui.run('renderTrends()');
  assert.match(ui.element('panel-history').innerHTML, /No saved daily usage/);
  ui.run('S.dailyError = true; renderTrends()');
  assert.match(ui.element('panel-history').innerHTML, /Could not load saved daily history/);
  assert.doesNotMatch(ui.element('panel-history').innerHTML, /No saved daily usage/);
});

test('History controls persist through rerenders and export only the selected daily rows', () => {
  const ui = dashboard([row(today, 10, 0), row(today, 30, 1, 'claude'), row('2026-01-01', 90)], { range: '7' });
  ui.run('renderTrends()');
  ui.element('#trends-provider').change({ target: { value: 'codex' } });
  ui.element('#trends-group').change({ target: { value: 'week' } });
  ui.run('renderTrends(); downloadText = (name, content) => { fixture = { name, content }; };');
  assert.match(ui.element('panel-history').innerHTML, /value="codex" selected/);
  assert.match(ui.element('panel-history').innerHTML, /value="week" selected/);
  ui.element('#trends-export').click();
  const exported = ui.run('fixture');
  assert.equal(exported.name, 'usagestat-history-2026-08-31-2026-09-06.csv');
  assert.equal(exported.content.split('\r\n').length, 2);
  assert.match(exported.content, /2026-09-06,codex/);
  assert.match(exported.content, /0\.000000/);
  assert.doesNotMatch(exported.content, /claude|2026-01-01/);
});

test('snapshot buckets use latest counters and peak quota, including counter resets', () => {
  const ui = dashboard();
  ui.context.samples = [
    { ts: today+'T09:00:00Z', inputTokens: 100, totalTokens: 100, cost: 5, primaryPercent: 80 },
    { ts: today+'T10:00:00Z', inputTokens: 100, totalTokens: 100, cost: 5, primaryPercent: 80 },
    { ts: today+'T11:00:00Z', inputTokens: 20, totalTokens: 20, cost: 1, primaryPercent: 10 },
  ];
  const result = ui.run("aggregateHistory(samples, 'day')");
  assert.equal(result.length, 1);
  assert.equal(result[0].cost, 1);
  assert.equal(result[0].inputTokens, 20);
  assert.equal(result[0].primaryPercent, 80);
  assert.equal(ui.run("aggregateHistory(samples.slice().reverse(), 'day')[0].cost"), 1);
});

test('daemon daily responses retain costs and token components after UI normalization', () => {
  const ui = dashboard();
  ui.context.payload = { daily: [{ providerId: 'codex', displayName: 'Codex', date: today,
    inputTokens: 10, outputTokens: 5, cacheReadTokens: 8, cacheCreationTokens: 2,
    reasoningOutputTokens: 3, totalTokens: 28, costUsd: 1.25, source: 'ccusage' }] };
  ui.run('S.dailyRows = normalizeDailyRows(payload)');
  const result = ui.run('UsageTrends.summarize(S.dailyRows, {range:"7"})');
  assert.equal(result.current.cost, 1.25);
  assert.equal(result.current.totalTokens, 28);
  assert.equal(result.current.cacheReadTokens, 8);
  assert.equal(result.current.reasoningOutputTokens, 3);
});

test('large history charts combine buckets without dropping usage totals', () => {
  const ui = dashboard();
  ui.context.buckets = Array.from({ length: 365 }, (_, i) => ({
    start: new Date(Date.UTC(2025, 0, 1+i)).toISOString().slice(0,10),
    end: new Date(Date.UTC(2025, 0, 1+i)).toISOString().slice(0,10),
    cost: 1, days: 1, recordedDays: 1,
  }));
  const svg = ui.run('trendsChart(buckets, "cost")');
  const totals = [...svg.matchAll(/<title>\d{4}[^<]*?: \$(\d+\.\d+)/g)].map(m => Number(m[1]));
  assert.equal(totals.length, 122);
  assert.equal(totals.reduce((a,b) => a+b, 0), 365);
});

test('token displays promote rounded units and preserve ordinary precision and signs', () => {
  const ui = dashboard();
  for (const [value, expected] of [
    [0, '0'], [999, '999'], [1234, '1.2K'], [999949, '999.9K'],
    [999950, '1.0M'], [999999, '1.0M'], [1234567, '1.2M'],
    [999949999, '999.9M'], [999950000, '1.00B'], [999999999, '1.00B'],
    [1234567890, '1.23B'], [Number.MAX_SAFE_INTEGER, '9007199.25B'],
  ]) {
    ui.context.tokenValue = value;
    assert.equal(ui.run('fmtTok(tokenValue)'), expected);
    if (value) assert.equal(ui.run('fmtTok(-tokenValue)'), '-' + expected);
  }
  for (const value of [NaN, Infinity, -Infinity, null, undefined]) {
    ui.context.tokenValue = value;
    assert.equal(ui.run('fmtTok(tokenValue)'), '—');
  }
});

test('Kimi exhausted monthly pool reaches the overview primary percentage', () => {
  const { load, response } = require('../../../tests/provider-sync-harness.cjs');
  const provider = load('kimi', { provider: { apiKey: 'fixture', cookieHeader: 'fixture' },
    request: req => req.method === 'GET'
      ? response({ usage: { used: 0, limit: 100 }, limits: [{ window: { duration: 300, timeUnit: 'TIME_UNIT_MINUTE' }, detail: { used: 1, limit: 100 } }] })
      : response({ subscriptionBalance: { amountUsedRatio: 1, expireTime: '2026-10-01T00:00:00Z' } }) });
  const ui = dashboard();
  ui.context.snapshot = { providerId: 'kimi', metrics: provider.probe().lines };
  assert.equal(ui.run('primaryPercentOf(snapshot)'), 100);
});
