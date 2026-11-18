const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const trends = require('../src/dashboard-trends.js');
const settings = require('../src/dashboard-settings.js');

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

test('month-to-date is calendar-aligned and preserves independent completeness flags',()=>{
  const result=trends.summarize([row('2026-08-31',100),{...row('2026-09-01',10,0),costKnown:false},row(today,5,2)],{range:'month'},today);
  assert.equal(result.range.start,'2026-09-01');
  assert.equal(result.current.totalTokens,15);
  assert.equal(result.current.tokensKnown,true);
  assert.equal(result.current.costKnown,false);
  assert.equal(result.current.cost,2);
  assert.equal(result.buckets[0].costKnown,false);
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
  assert.match(html, /src="\/dashboard\/settings\.js"/);
  for (const script of html.matchAll(/<script>([\s\S]*?)<\/script>/g)) new vm.Script(script[1]);
});

function dashboard(rows = [], prefs = {}) {
  const html = fs.readFileSync(require.resolve('../src/dashboard.html'), 'utf8');
  const script = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)][0][1].replace(/load\(\);\s*$/, '');
  const elements = new Map();
  const element = id => {
    if (!elements.has(id)) elements.set(id, { style: {}, innerHTML: '',
      querySelector: selector => element(selector), querySelectorAll: () => [],
      addEventListener: (event, fn) => { element(id)[event] = fn; } });
    return elements.get(id);
  };
  const context = vm.createContext({ UsageTrends: trends, UsageDashboardSettings: settings, fixture: rows,
    setInterval: () => 0, localStorage: { getItem: () => JSON.stringify({ trends: prefs }), setItem: () => {} },
    document: { getElementById: element } });
  vm.runInContext(script, context);
  vm.runInContext("S.dailyRows = fixture; todayUtcDate = () => new Date('2026-09-06T00:00:00Z');", context);
  // Inject the clock into the pure model, whose default otherwise uses real time.
  context.UsageTrends = { ...trends, summarize: (rows, options) => trends.summarize(rows, options, today) };
  return { context, element, run: code => vm.runInContext(code, context) };
}

test('daily model detail uses UTC rows and never fetches session reports', async () => {
  const ui = dashboard([row('2026-09-05', 15, 2), row(today, 20, 0)], {range:'7', breakdown:'model'});
  const urls = [];
  ui.context.fetch = async url => {
    urls.push(url);
    return {ok:true, json:async()=>({daily:[
      {...row('2026-09-05',15,2),models:{'priced':{totalTokens:15,costUsd:2,costKnown:true,sessions:1}}},
      {...row(today,20,0),models:{'unknown':{totalTokens:20,costUsd:0,costKnown:false}}},
      {...row('2026-08-01',999,999),models:{'outside-range':{totalTokens:999,costUsd:999,costKnown:true}}},
    ]})};
  };
  ui.run('renderTrends()');
  await new Promise(resolve=>setImmediate(resolve));
  ui.run('renderTrends()');
  assert.deepEqual(urls, ['/v1/history/models/codex']);
  const html=ui.element('panel-history').innerHTML;
  assert.match(html,/priced/);
  assert.match(html,/Unpriced/);
  assert.match(html,/Session-days/);
  assert.doesNotMatch(html,/outside-range/);
  assert.match(html,/actual UTC day/);
});

test('zero model costs with explicit coverage stay known and missing sessions stay unknown',()=>{
  const ui=dashboard([row(today,5,0)],{range:'7',breakdown:'model'});
  ui.run(`ensureModelReports=()=>{};S.modelReports={codex:{status:'ok',rows:[{providerId:'codex',date:'${today}',models:{free:{totalTokens:5,costUsd:0,costKnown:true}}}]}};renderTrends()`);
  const html=ui.element('panel-history').innerHTML;
  assert.match(html,/free/);
  assert.doesNotMatch(html,/Unpriced/);
  assert.match(html,/<td>—<\/td><\/tr>/);
});

test('opening provider analytics requests only daily reports and bounded chart history',async()=>{
  const ui=dashboard();
  const urls=[];
  ui.context.fetch=async url=>{urls.push(url);return {ok:true,json:async()=>[]};};
  await ui.run("fetchCcusageReports('codex')");
  await ui.run("fetchHistory('codex')");
  assert.equal(urls.length,4);
  assert.ok(urls.every(url=>!url.includes('/session')&&!url.includes('/blocks')));
  assert.equal(urls.at(-1),'/v1/history/codex?since=2026-08-08&group=hour&view=chart');
});

test('session and cache-savings totals preserve absent values',()=>{
  const known={...row(today,10,1),sessions:2,cacheSavingsUsd:0.4};
  let total=trends.summarize([known],{},today).current;
  assert.equal(total.sessions,2);
  assert.equal(total.cacheSavingsUsd,0.4);
  assert.ok(total.sessionsKnown&&total.cacheSavingsKnown);
  total=trends.summarize([known,row('2026-09-05',5)],{},today).current;
  assert.equal(total.sessionsKnown,false);
  assert.equal(total.cacheSavingsKnown,false);
});

test('component costs survive normalization, grouping and CSV without inventing missing values',()=>{
  const ui=dashboard();
  ui.context.payload={daily:[{providerId:'codex',date:today,costUsd:1,
    inputCostUsd:.2,cacheReadCostUsd:.1,cacheWriteCostUsd:0,outputCostUsd:.7}]};
  ui.run('S.dailyRows=normalizeDailyRows(payload)');
  const normalized=ui.run('S.dailyRows[0]');
  assert.equal(normalized.cacheWriteCostUsd,0);
  const total=trends.totals([normalized,normalized]);
  assert.equal(total.costComponentsKnown,true);
  assert.equal(total.inputCostUsd,.4);
  assert.equal(total.outputCostUsd,1.4);
  const mixed=trends.totals([normalized,row(today,10,2)]);
  assert.equal(mixed.cost,3);
  assert.equal(mixed.costKnown,true);
  assert.equal(mixed.costComponentsKnown,false);
  assert.equal(mixed.inputCostUsd,null);
  const unknown=trends.totals([{...normalized,costKnown:false}]);
  assert.equal(unknown.outputCostUsd,null);
  const mismatched=trends.totals([{...normalized,outputCostUsd:10}]);
  assert.equal(mismatched.costComponentsKnown,false);
  ui.run('downloadText=(name,content)=>{fixture=content};exportDailyCsv()');
  const [header,line]=ui.run('fixture').split('\r\n').map(line=>line.split(','));
  assert.equal(line[header.indexOf('cache_write_cost_usd')],'0');
  assert.equal(line[header.indexOf('output_cost_usd')],'0.7');
  ui.context.payload={daily:[{providerId:'codex',date:today,costUsd:1}]};
  ui.run('S.dailyRows=normalizeDailyRows(payload);exportDailyCsv()');
  assert.equal(ui.run('S.dailyRows[0].inputCostUsd'),null);
  assert.equal(ui.run('fixture').split('\r\n')[1].split(',')[header.indexOf('input_cost_usd')],'');
});

test('portable preferences exclude credentials, consent and arbitrary nested state',()=>{
  const ui=dashboard();
  const prefs=ui.run(`cleanPreferences({quotaDisplay:'remaining',apiKey:'secret',cookieHeader:'secret',consent:true,
    trends:{range:'month',token:'secret'},__proto__:{polluted:true}})`);
  assert.equal(prefs.quotaDisplay,'remaining');
  assert.equal(prefs.trends.range,'month');
  assert.doesNotMatch(JSON.stringify(prefs),/secret|consent|polluted/);
});

test('unpriced history never renders as zero dollars and remaining does not invert warning colors',()=>{
  const ui=dashboard([{...row(today,20,0),costKnown:false}]);
  ui.run('renderTrends()');
  assert.match(ui.element('panel-history').innerHTML,/Unpriced/);
  assert.equal(ui.run("S.prefs.quotaDisplay='remaining';fmtMetric({used:90,limit:100,format:{kind:'percent'}})"),'10.0% remaining');
  assert.equal(ui.run('barCol(90)'),'#ff5f57');
});

test('partial history totals cannot produce precise provider shares',()=>{
  const ui=dashboard([{...row(today,20,2),costKnown:false},row(today,10,1,'claude')]);
  ui.run('renderTrends()');
  const html=ui.element('panel-history').innerHTML;
  assert.doesNotMatch(html,/[\d.]+% of cost|<td>[\d.]+%<\/td>/);
  assert.equal(ui.run("metricShare({cost:1},{cost:3},'cost')"),'33.3%');
  assert.equal(ui.run("metricShare({totalTokens:1},{totalTokens:3,tokensKnown:false},'totalTokens')"),'—');
});

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
  ui.element('panel-history').onclick({ target: { closest: () => ({ dataset: { set: 'group', value: 'week' } }) } });
  ui.run('renderTrends(); downloadText = (name, content) => { fixture = { name, content }; };');
  assert.match(ui.element('panel-history').innerHTML, /value="codex" selected/);
  assert.match(ui.element('panel-history').innerHTML, /data-set="group" data-value="week" aria-pressed="true"/);
  assert.match(ui.element('panel-history').innerHTML, /Weekly/, 'the chart honours the chosen grouping');
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

test('snapshot bucket timestamps and labels retain UTC across browser time zones',()=>{
  const original=process.env.TZ;
  try {
    for(const zone of ['Europe/Amsterdam','America/Los_Angeles','Asia/Kathmandu']){
      process.env.TZ=zone;
      const ui=dashboard();
      const bucket=ui.run("bucketKey('2026-09-06T00:10:00Z','hour')");
      assert.equal(new Date(bucket).toISOString(),'2026-09-06T00:00:00.000Z',zone);
      assert.equal(ui.run("fmtWhen(Date.parse('2026-09-06T00:00:00Z'),'day')"),'Sep 6',zone);
      assert.equal(ui.run("fmtWhen(Date.parse('2026-09-07T00:00:00Z'),'week')"),'Week of Sep 7',zone);
    }
  } finally {if(original===undefined)delete process.env.TZ;else process.env.TZ=original;}
});

test('quota aggregation treats inherited object keys as ordinary labels',()=>{
  const ui=dashboard();
  const progress=ui.run(`aggregateHistory([{ts:'${today}T00:00:00Z',progress:
    ['__proto__','constructor','toString'].map(label=>({label,percent:40,used:40}))}],'day')[0].progress`);
  assert.deepEqual(Array.from(progress,p=>p.label),['__proto__','constructor','toString']);
  assert.ok(progress.every(p=>p.percent===40));
  assert.equal(ui.run("Object.prototype.hasOwnProperty('percent')"),false);
});

test('known zero quotas remain recorded history',()=>{
  const ui=dashboard();
  ui.run(`timeLineChart=(container,series)=>{fixture=series};buildHistoryChart('history',
    [{ts:'${today}T00:00:00Z',progress:[{label:'Quota',format:'percent',percent:0}]}],'quota','raw')`);
  assert.equal(ui.run('fixture[0].pts[0].v'),0);
  assert.doesNotMatch(ui.element('history').innerHTML,/No quota history/);
  ui.run(`fixture=null;buildHistoryChart('missing',normalizeHistory(
    [{providerId:'codex',ts:'${today}T00:00:00Z',primaryPercent:null,progress:[]}]),'quota','day')`);
  assert.equal(ui.run('fixture'),null);
  assert.match(ui.element('missing').innerHTML,/No quota history/);
});

test('saved model mix excludes future dates, escapes names, and retains partial cost',()=>{
  const ui=dashboard();
  ui.context.modelRows=[
    {date:today,models:{'<img src=x onerror=alert(1)>':{costUsd:2,costKnown:false,totalTokens:10}}},
    {date:'2026-09-07',models:{future:{costUsd:100,costKnown:true,totalTokens:100}}},
    {date:'2026-08-07',models:{tooOld:{costUsd:100,costKnown:true,totalTokens:100}}},
  ];
  ui.run("S.modelReports={codex:{status:'ok',at:Date.now(),rows:modelRows}};");
  const html=ui.run("savedModelMix('codex')");
  assert.doesNotMatch(html,/future|tooOld|<img/);
  assert.match(html,/&lt;img/);
  assert.match(html,/\$2\.00\+/);
});

test('saved model mix clears an empty response but retains saved data after a failed refresh',async()=>{
  for(const ok of [true,false]){
    const ui=dashboard();let finish;
    ui.context.fetch=()=>new Promise(resolve=>{finish=resolve;});
    ui.run(`S.modelReports={codex:{status:'ok',at:0,rows:[{date:'${today}',models:{old:{costUsd:1,costKnown:true}}}]}}`);
    ui.element('model-mix-codex').innerHTML=ui.run("savedModelMix('codex')");
    assert.match(ui.element('model-mix-codex').innerHTML,/old/);
    finish({ok,json:async()=>({daily:[]})});
    await new Promise(resolve=>setImmediate(resolve));
    if(ok){assert.doesNotMatch(ui.element('model-mix-codex').innerHTML,/model-label">old/);assert.match(ui.element('model-mix-codex').innerHTML,/No saved model history/);}
    else assert.match(ui.element('model-mix-codex').innerHTML,/model-label">old/);
  }
});

test('heatmap distinguishes missing, known zero, unknown and partial token counts',()=>{
  const ui=dashboard();
  const html=ui.run(`renderHeatmap([{date:'2026-09-06',totalTokens:0,tokensKnown:false},
    {date:'2026-09-05',totalTokens:10,tokensKnown:false},{date:'2026-09-04',totalTokens:0,tokensKnown:true}])`);
  assert.match(html,/Sep 6: Unknown tokens/);
  assert.match(html,/Sep 5: 10\+ tokens/);
  assert.match(html,/Sep 4: 0 tokens/);
  assert.match(html,/Sep 3: no record/);
});

test('heatmap span controls change the calendar width and persist per provider',()=>{
  const ui=dashboard();
  ui.run(`todayUtcDate=()=>new Date('2026-09-06T00:00:00Z');`);
  const rows="[{date:'2025-06-02',totalTokens:5,tokensKnown:true},{date:'2026-09-05',totalTokens:10,tokensKnown:true}]";
  const weeks=html=>Number(html.match(/--weeks:(\d+)/)[1]);
  const six=ui.run(`heatmapSectionHtml('codex',${rows})`);
  assert.equal(weeks(six),26);
  assert.match(six,/Activity · last 6 months/);
  assert.match(six,/data-heatmap-span="26" aria-pressed="true"/);
  assert.equal(weeks(ui.run(`S.heatmapSpans={codex:'52'};heatmapSectionHtml('codex',${rows})`)),52);
  const all=ui.run(`S.heatmapSpans={codex:'all'};heatmapSectionHtml('codex',${rows})`);
  assert.ok(weeks(all)>=66,'All reaches the earliest saved day');
  assert.match(all,/Jun 2: 5 tokens/);
  assert.match(all,/Jan '26/);
  assert.equal(weeks(ui.run(`heatmapSectionHtml('claude',${rows})`)),26,'other providers keep the default');
});

test('timeline gradient identifiers cannot inject markup and series labels are escaped',()=>{
  const ui=dashboard();
  const svg={setAttribute:()=>{},querySelector:()=>({children:[]}),addEventListener:()=>{}};
  ui.context.chart={id:'chart-\"><svg onload="alert(1)">',clientWidth:860,innerHTML:'',querySelector:()=>svg};
  ui.run(`timeLineChart(chart,[{label:'<img src=x onerror=alert(1)>',color:'#fff',fmt:String,pts:[{t:0,v:10},{t:1000,v:20}]},
    {label:'Safe',color:'#eee',fmt:String,pts:[{t:0,v:5},{t:1000,v:10}]}],{max:100,peak:true,axis:String,when:String})`);
  assert.doesNotMatch(ui.context.chart.innerHTML,/<svg onload|<img/);
  assert.match(ui.context.chart.innerHTML,/&lt;img/);
});

test('an API transport source does not imply billing authority',()=>{
  const ui=dashboard();
  assert.equal(ui.run("costSourceLabel({source:'api'})"),'api');
  assert.equal(ui.run("costSourceLabel({costSource:'api-rate-estimate'})"),'local estimate');
  assert.equal(ui.run("costSourceLabel({source:'billing'})"),'billing');
});

test('a single-day History range has a visible data marker',()=>{
  const ui=dashboard();
  const svg=ui.run(`trendsChart([{start:'${today}',end:'${today}',cost:2,days:1,recordedDays:1}],
    'cost',[{id:'codex',name:'Codex',color:'#123456',values:[2]}])`);
  assert.match(svg,/<circle cx="[\d.]+" cy="[\d.]+" r="3" fill="#123456"/);
});

test('provider identifiers are escaped in provider panels and cost controls',async()=>{
  const ui=dashboard();
  ui.context.providerId='x\"><svg onload="alert(1)">';
  ui.run(`S.snapshots=[{providerId,displayName:'Safe',metrics:[]}];
    animateBarsAndArcs=()=>{};animateCompareBars=()=>{};fetchHistory=async()=>[];
    renderStatsSection=()=>{};renderHistorySection=()=>{};fetchCcusageReports=async()=>({});
    fetchCost=async()=>({});renderCostSection=()=>{}`);
  await ui.run('renderProvider(providerId)');
  const panel=ui.element('panel-'+ui.context.providerId).innerHTML;
  assert.doesNotMatch(panel,/<svg onload/);
  assert.match(panel,/id="cost-box-x&quot;&gt;&lt;svg/);
  assert.doesNotMatch(ui.run('costHeaderHtml(providerId)'),/<svg onload/);
});

test('chart values keep unknown and partial accounting flags',()=>{
  const ui=dashboard();
  assert.equal(ui.run("fmtMetricValue('cost',0,false)"),'Unpriced');
  assert.equal(ui.run("fmtMetricValue('cost',10,false)"),'$10.00+');
  assert.equal(ui.run("fmtMetricValue('outputTokens',0,false)"),'Unknown');
  assert.equal(ui.run("recordedMetric({outputTokens:0,tokensKnown:false},'outputTokens')"),'Unknown');
  const svg=ui.run(`trendsChart([{start:'${today}',end:'${today}',cost:0,costKnown:false,days:1,recordedDays:1}],'cost')`);
  assert.match(svg,/: Unpriced ·/);
  assert.doesNotMatch(svg,/: \$0\.00 ·/);
});

test('cost without a token breakdown stays visible as unallocated',()=>{
  const ui=dashboard();
  const parts=ui.run("costParts({costUsd:10,totalTokens:100},costWeights('codex'))");
  assert.equal(parts.cost.unallocated,10);
  assert.equal(parts.cost.input+parts.cost.cacheRead+parts.cost.cacheWrite+parts.cost.output,0);
  const exact=ui.run("costParts({costUsd:10,inputCostUsd:10,cacheReadCostUsd:0,cacheWriteCostUsd:0,outputCostUsd:0},costWeights('codex'))");
  assert.equal(exact.exact,true);
  assert.equal(exact.cost.input,10);
});

test('local-log extras ignore rows that did not come from local transcripts',()=>{
  const ui=dashboard();
  const extras=ui.run(`S.modelReports={codex:{rows:[{date:'${today}',costSource:'api-rate-estimate',sessions:2,cacheSavingsUsd:1}]},
    claude:{rows:[{date:'${today}',source:'billing',sessions:3,cacheSavingsUsd:5}]}};
    localLogExtras({providers:[{id:'codex',current:{rows:1}},{id:'claude',current:{rows:1}}],
      rows:[{providerId:'codex',date:'${today}'},{providerId:'claude',date:'${today}'}]})`);
  assert.equal(extras.sessions,2);
  assert.equal(extras.sessionDays,1);
  assert.equal(extras.days,2);
  assert.equal(extras.savings,1);
});

test('missing quota samples break the timeline instead of bridging it',()=>{
  const ui=dashboard();
  ui.context.chart={id:'c',clientWidth:860,innerHTML:'',querySelector:()=>({setAttribute(){},addEventListener(){},querySelector:()=>({children:[],setAttribute(){}}),getBoundingClientRect:()=>({left:0,width:860})})};
  ui.run(`timeLineChart(chart,[{label:'Quota',color:'#123',fmt:String,pts:[
    {t:Date.UTC(2026,8,1,0),v:10},{t:Date.UTC(2026,8,1,1),v:12},{t:Date.UTC(2026,8,1,2),v:null},
    {t:Date.UTC(2026,8,1,3),v:20},{t:Date.UTC(2026,8,1,4),v:22}]}],{max:100,axis:String,when:String})`);
  const strokes=[...ui.context.chart.innerHTML.matchAll(/stroke="#123" stroke-width="1.8"/g)];
  assert.equal(strokes.length,2);
});

test('an Error badge marks a provider as needing attention',()=>{
  const ui=dashboard();
  assert.equal(ui.run("snapshotHasError({source:'local',metrics:[{type:'badge',label:'Error',text:'not a function',color:'red'}]})"),true);
  assert.equal(ui.run("snapshotHasError({source:'error',metrics:[]})"),true);
  assert.equal(ui.run("snapshotHasError({source:'local',metrics:[{type:'badge',label:'Status',text:'ok'}]})"),false);
});

test('growth from a near-empty period reads as a multiple',()=>{
  const ui=dashboard();
  assert.equal(ui.run("trendChange({totalTokens:3247000,rows:1},{totalTokens:10,rows:1},'totalTokens')"),'324,700× previous period');
  assert.equal(ui.run("trendChange({totalTokens:20,rows:1},{totalTokens:10,rows:1},'totalTokens')"),'+100.0% vs previous period');
});

test('Sources compares retained sources per provider-day and marks the headline source',()=>{
  const ui=dashboard();
  ui.run(`S.sourceReport={status:'ok',at:Date.now(),rows:[{providerId:'<codex>',displayName:'<Codex>',date:'${today}',sourceRows:[
    {source:'ccusage',selected:true,costUsd:10,costKnown:true,totalTokens:100,tokensKnown:true},
    {source:'local-transcript-v2',costSource:'api-rate-estimate',selected:false,costUsd:12.5,costKnown:true,totalTokens:120,tokensKnown:true}]}]}`);
  const html=ui.run(`sourcesBreakdown({range:{start:'${today}',end:'${today}'}})`);
  assert.match(html,/\+\$2\.50/);
  assert.match(html,/ccusage \$10\.00 vs local estimate \$12\.50/);
  assert.match(html,/\$10\.00 <span class="u-sel"/);
  assert.match(html,/&lt;Codex&gt;/);
  assert.doesNotMatch(html,/<Codex>/);
});

test('partial pricing shows an approximate share of priced cost and registry names win',()=>{
  const ui=dashboard([{...row(today,20,3),costKnown:false},row(today,10,1,'claude')]);
  ui.run("S.providers=[{id:'codex',name:'Codex'},{id:'claude',name:'Claude'}]; renderTrends()");
  const html=ui.element('panel-history').innerHTML;
  assert.match(html,/≈75\.0% of priced cost/);
  assert.doesNotMatch(html,/[\d.]+% of cost/);
  assert.match(html,/>Codex</);
});

test('inactive providers are hidden until the user chooses to show them',()=>{
  const ui=dashboard([row(today,10,1,'codex')]);
  ui.run(`S.snapshots=[
    {providerId:'codex',displayName:'Codex',metrics:[{type:'progress',label:'Session',used:10,limit:100,format:{kind:'percent'}}]},
    {providerId:'gemini',displayName:'Gemini',source:'local',metrics:[{type:'badge',label:'Error',text:'Gemini session expired'}]},
    {providerId:'claude',displayName:'Claude',source:'error',metrics:[]}];
    S.providers=[{id:'codex',name:'Codex'},{id:'gemini',name:'Gemini'},{id:'claude',name:'Claude'}];`);
  // Claude errors but has tracked usage in the last 30 days, so it stays visible.
  ui.run(`S.dailyRows.push({providerId:'claude',date:'${today}',totalTokens:5,cost:0,inputTokens:5,outputTokens:0,cacheReadTokens:0,cacheCreationTokens:0,reasoningOutputTokens:0})`);
  assert.deepEqual(Array.from(ui.run('visibleSnapshots()'),s=>s.providerId),['codex','claude']);
  assert.deepEqual(Array.from(ui.run('hiddenInactive()'),s=>s.providerId),['gemini']);
  ui.run("S.prefs.inactive='show'");
  assert.equal(ui.run('visibleSnapshots().length'),3);
  assert.equal(ui.run('hiddenInactive().length'),0);
  assert.equal(ui.run("cleanPreferences({inactive:'show'}).inactive"),'show');
});

test('provider refresh retains the previous panel while new analytics are pending', async()=>{
 const ui=dashboard();
 ui.element('panel-codex').innerHTML='<article>Existing cost $12</article>';
 ui.run(`S.snapshots=[{providerId:'codex',metrics:[]}];fetchHistory=()=>new Promise(()=>{});fetchCost=()=>new Promise(()=>{});fetchCcusageReports=()=>new Promise(()=>{});renderProvider('codex');`);
 assert.equal(ui.element('panel-codex').innerHTML,'<article>Existing cost $12</article>');
});

test('partial model amounts contribute to the priced-cost share denominator',()=>{
 const ui=dashboard();ui.element('model-table-codex').dataset={};
 ui.run(`S.modelReports={codex:{status:'ok',at:Date.now(),rows:[{date:'${today}',models:{astra:{costUsd:80,costKnown:true},sol:{costUsd:20,costKnown:false}}}]}}; updateModelTable('codex',[{date:'${today}'}]);`);
 const html=ui.element('model-table-codex').innerHTML;
 assert.match(html,/80\.0%/);assert.match(html,/≈20\.0%/);assert.doesNotMatch(html,/100\.0%/);
});

test('failed cost and history refreshes preserve their last successful responses',async()=>{
 const ui=dashboard();ui.context.fetch=async()=>({ok:false});
 ui.run(`S.costs.codex={daily:[{date:'${today}',costUsd:3}]};S.histories.codex=[{ts:'${today}',value:5}];`);
 await ui.run(`Promise.all([fetchCost('codex'),fetchHistory('codex')])`);
 assert.equal(ui.run('S.costs.codex.daily[0].costUsd'),3);
 assert.equal(ui.run('S.histories.codex[0].value'),5);
});

test('History keeps the previous populated totals when a refresh fails',()=>{
 const ui=dashboard([row(today,10,12)]);
 ui.run('S.dailyError=true;renderTrends()');
 assert.match(ui.element('panel-history').innerHTML,/\$12\.00/);
 assert.match(ui.element('panel-history').innerHTML,/showing the last successful data/);
});

test('an older cost response cannot replace a newer retained response',async()=>{
 const ui=dashboard(),pending=[];ui.context.fetch=()=>new Promise(resolve=>pending.push(resolve));
 const old=ui.run("fetchCost('codex')"), fresh=ui.run("fetchCost('codex')");
 pending[1]({ok:true,json:async()=>({value:20})});await fresh;
 pending[0]({ok:true,json:async()=>({value:10})});await old;
 assert.equal(ui.run('S.costs.codex.value'),20);
});

test('a first provider visit shows its snapshot while analytics are pending',()=>{
 const ui=dashboard();
 ui.run(`S.snapshots=[{providerId:'codex',metrics:[]}];fetchHistory=()=>new Promise(()=>{});fetchCost=()=>new Promise(()=>{});fetchCcusageReports=()=>new Promise(()=>{});renderProvider('codex');`);
 assert.match(ui.element('panel-codex').innerHTML,/Current Usage/);
});

test('a missing snapshot invalidates an earlier pending provider render',async()=>{
 const ui=dashboard();let finish;ui.context.dataReady=new Promise(resolve=>{finish=resolve;});
 const pending=ui.run(`S.snapshots=[{providerId:'codex',metrics:[]}];fetchHistory=()=>dataReady.then(()=>[]);fetchCost=()=>dataReady.then(()=>null);fetchCcusageReports=()=>dataReady.then(()=>({}));renderProvider('codex');`);
 await ui.run(`S.snapshots=[];renderProvider('codex')`);
 finish();await pending;
 assert.match(ui.element('panel-codex').innerHTML,/No data yet/);
 assert.doesNotMatch(ui.element('panel-codex').innerHTML,/Current Usage/);
});

test('overlapping provider refreshes share the pending analytics request',()=>{
 const ui=dashboard();let scans=0;
 ui.context.pendingAnalytics=()=>{scans++;return new Promise(()=>{});};
 ui.run(`S.snapshots=[{providerId:'codex',metrics:[]}];fetchHistory=pendingAnalytics;fetchCost=pendingAnalytics;fetchCcusageReports=pendingAnalytics;renderProvider('codex');renderProvider('codex');`);
 assert.equal(scans,3);
 assert.match(ui.element('panel-codex').innerHTML,/Current Usage/);
});

test('initial analytics use accessible skeletons rather than empty loading text',()=>{
 const ui=dashboard();
 const html=ui.run("loadingSkeleton('Loading cost history')");
 assert.match(html,/role="status"/);assert.match(html,/aria-hidden="true"/);
 assert.match(html,/sk-chart/);assert.match(html,/sk-metrics/);
 assert.match(ui.run("loadingSkeleton('Loading usage analytics','analytics')"),/sk-split/);
 assert.match(ui.run("loadingSkeleton('Loading usage history','history')"),/sk-area/);
 assert.match(ui.run("loadingSkeleton('Loading local usage','table')"),/sk-rows/);
});

test('pending provider sections keep the titles of the sections they become',()=>{
 const ui=dashboard();
 ui.run(`S.snapshots=[{providerId:'codex',metrics:[]}];fetchHistory=()=>new Promise(()=>{});fetchCost=()=>new Promise(()=>{});fetchCcusageReports=()=>new Promise(()=>{});renderProvider('codex');`);
 const html=ui.element('panel-codex').innerHTML;
 assert.match(html,/Usage Analytics/);assert.match(html,/Usage History/);assert.match(html,/Cost History · Last 30 Days/);
 assert.doesNotMatch(html,/Deep Statistics/);
});

test('cost range survives a refreshed cost response',()=>{
 const ui=dashboard();ui.element('cost-box-codex').querySelectorAll=()=>[];ui.run("S.costRanges={codex:'90'};updateCostView=()=>{};setupCostRange(document.getElementById('cost-box-codex'),'codex',[])");
 assert.equal(ui.element('cost-box-codex')._costState.range,'90');
});

test('model history participates in the background refresh barrier',async()=>{
 const ui=dashboard();ui.element('panel-codex').innerHTML='<article>Existing models</article>';
 ui.context.fetch=()=>{};
 ui.run(`S.snapshots=[{providerId:'codex',metrics:[]}];fetchHistory=async()=>[];fetchCost=async()=>null;fetchCcusageReports=async()=>({});ensureModelReports=()=>new Promise(()=>{});renderProvider('codex');`);
 await new Promise(resolve=>setImmediate(resolve));
 assert.equal(ui.element('panel-codex').innerHTML,'<article>Existing models</article>');
});

test('background redraw paints quota bars immediately instead of collapsing to zero',()=>{
 const ui=dashboard(),bar={style:{},dataset:{w:'70%'}};
 ui.context.barRoot={querySelectorAll:selector=>selector.startsWith('.bar')?[bar]:[]};
 ui.run('animateBarsAndArcs(barRoot,false)');
 assert.equal(bar.style.width,'70%');assert.equal(bar.style.transition,'none');
});

test('usage history charts tokens from saved daily usage, summed per group', () => {
  const ui = dashboard();
  ui.context.days = [row('2026-08-31', 10, 1), row('2026-09-02', 20, 2), row(today, 40, 4), row('2026-08-01', 5, .5)];
  ui.run(`fixture=null;timeLineChart=(container,series)=>{fixture=series};
    buildHistoryChart('weekly', usageHistoryRecords(usageRangeRows(days.map(normalizeCostRow), 7), 'week'), 'tokens', 'week')`);
  const total = ui.run("fixture.find(s=>s.key==='totalTokens').pts.map(p=>p.v)");
  assert.deepEqual([...total], [70], 'week of Aug 31 sums its days; days outside the span are excluded');
  const daily = ui.run("usageHistoryRecords(usageRangeRows(days.map(normalizeCostRow), 7), 'day').map(r=>r.totalTokens)");
  assert.deepEqual([...daily], [10, 0, 20, 0, 0, 0, 40], 'idle days plot as zero instead of bridging');
  const monthly = ui.run("usageHistoryRecords(usageRangeRows(days.map(normalizeCostRow), null), 'month').map(r=>[r.ts.slice(0,7),r.cost])");
  assert.deepEqual(JSON.parse(JSON.stringify(monthly)), [['2026-08', 1.5], ['2026-09', 6]]);
});

test('usage history token views fall back to daily groups and quota keeps raw samples', () => {
  const ui = dashboard();
  assert.equal(ui.run("historyGroupFor('tokens','raw')"), 'day');
  assert.equal(ui.run("historyGroupFor('cost','month')"), 'month');
  assert.equal(ui.run("historyGroupFor('quota','raw')"), 'raw');
});

test('wider history spans request older quota samples', async () => {
  const ui = dashboard();
  const urls = [];
  ui.context.fetch = async url => { urls.push(url); return { ok: true, json: async () => [] }; };
  await ui.run("fetchHistory('codex', 90)");
  await ui.run("fetchHistory('codex', null)");
  assert.equal(urls[0], '/v1/history/codex?since=2026-06-09&group=hour&view=chart');
  assert.equal(urls[1], '/v1/history/codex?group=hour&view=chart');
});

test('cost history honours an explicit grouping over automatic coarsening', () => {
  const ui = dashboard();
  ui.context.days = Array.from({ length: 10 }, (_, i) => row(`2026-08-${String(20 + i).padStart(2, '0')}`, 10, 1));
  ui.run("buildChart('cost', days, 'codex', 'week')");
  assert.equal((ui.element('cost').innerHTML.match(/class="cost-col"/g) || []).length, 2);
  assert.match(ui.element('cost').innerHTML, /Grouped by week/);
  ui.run("buildChart('cost', days, 'codex', 'auto')");
  assert.equal((ui.element('cost').innerHTML.match(/class="cost-col"/g) || []).length, 10);
});

test('grouped chart axes label period starts and add the year across years', () => {
  const ui = dashboard();
  const months = ui.run("periodTickLabels(['2025-11-01','2025-12-01','2026-01-01'],'month')");
  assert.deepEqual([...months], ["NOV '25", 'DEC', "JAN '26"]);
  assert.deepEqual([...ui.run("periodTickLabels(['2026-08-31','2026-09-07'],'week')")], ['AUG 31', 'SEP 7']);
  assert.deepEqual([...ui.run('tickIndices(10, 4)')], [0, 3, 6, 9]);
  ui.context.samples = ['2026-05-01', '2026-06-01', '2026-07-01'].map((d, i) => ({ providerId: 'codex', ts: d + 'T00:00:00Z', primaryPercent: 10 * i }));
  // The DOM stub cannot wire hover handlers; the markup is written before that.
  ui.run("try{buildHistoryChart('monthly', normalizeHistory(samples), 'quota', 'month')}catch{}");
  const ticks = [...ui.element('monthly').innerHTML.matchAll(/font-size="10.5">([^<]+)<\/text>/g)].map(m => m[1]).filter(t => !/%$/.test(t));
  assert.deepEqual(ticks, ['MAY', 'JUN', 'JUL']);
  const marks = ui.run("timeAxisTicks(Date.UTC(2025,10,20), Date.UTC(2026,4,3), 'month', 6).map(m=>m.label)");
  assert.deepEqual([...marks], ['JAN', 'MAR', 'MAY'], 'month ticks align to calendar steps');
  const years = ui.run("timeAxisTicks(Date.UTC(2025,8,1), Date.UTC(2026,10,16), 'month', 6).map(m=>m.label)");
  assert.deepEqual([...years], ["OCT '25", "JAN '26", 'APR', 'JUL', 'OCT'], 'ticks crossing a year carry it');
  const days = ui.run("timeAxisTicks(Date.UTC(2026,10,9,17), Date.UTC(2026,10,16,17), 'hour', 6).map(m=>m.label)");
  assert.deepEqual([...days], ['NOV 11', 'NOV 13', 'NOV 15'], 'multi-day samples tick on evenly spaced UTC midnights');
});

test('timeline gaps are bridged faintly and marked as missing data',()=>{
  const ui=dashboard();
  ui.context.chart={id:'g',clientWidth:860,innerHTML:'',querySelector:()=>({setAttribute(){},addEventListener(){},querySelector:()=>({children:[],setAttribute(){}}),getBoundingClientRect:()=>({left:0,width:860})})};
  const hour=3600000, t0=Date.UTC(2026,8,1);
  ui.context.pts=[...Array.from({length:10},(_,i)=>({t:t0+i*hour,v:10+i})),...Array.from({length:10},(_,i)=>({t:t0+(100+i)*hour,v:30}))];
  ui.run(`timeLineChart(chart,[{label:'Quota',color:'#123',fmt:String,pts}],{max:100,axis:String,when:String})`);
  const html=ui.context.chart.innerHTML;
  assert.equal([...html.matchAll(/stroke="#123" stroke-width="1.8"/g)].length,2,'measured runs stay separate');
  assert.equal([...html.matchAll(/class="tl-bridge"/g)].length,1,'one dashed bridge spans the gap');
  assert.match(html,/NO DATA/);
});

test('hover details keep to the numbers; notes appear once pinned', () => {
  const ui = dashboard();
  ui.context.d = { title: 'Day', headline: { value: '$1.00' }, rows: [['Cost · USD', '$1.00']], note: 'Explanatory note.' };
  assert.doesNotMatch(ui.run('detailHtml(d,false)'), /Explanatory note/);
  assert.match(ui.run('detailHtml(d,true)'), /Explanatory note/);
  assert.doesNotMatch(ui.run('TIP.pinned=false;tipFooter()'), /press Enter/);
});

test('spend status projects the UTC month and alerts on budget pace and daily spikes',()=>{
  const ui=dashboard();
  // Sep 11 12:00 UTC: 10.5 days elapsed of 30.
  const now=Date.UTC(2026,8,11,12);
  ui.context.rows=[row('2026-08-31',0,30),row('2026-09-01',0,10),row('2026-09-10',0,11),row('2026-09-11',0,21),{...row('2026-09-11',0,0,'claude'),costKnown:false}];
  const status=ui.run(`spendStatus(rows, ${now})`);
  assert.equal(status.monthToDate,42,'August is excluded');
  assert.equal(status.today,21);
  assert.equal(status.costKnown,false,'an unpriced row this month marks the total partial');
  assert.equal(Math.round(status.projected),120);
  assert.equal(status.priorAverage,(30+10+11)/30);
  const keys=prefs=>ui.run(`spendAlerts(spendStatus(rows, ${now}), ${JSON.stringify(prefs)}).map(a=>a.key).join(",")`).split(",").filter(Boolean);
  assert.deepEqual(keys({spendBudgetUsd:0,spendSpikeMultiple:0}),[],'zero turns alerts off');
  assert.deepEqual(keys({spendBudgetUsd:100,spendSpikeMultiple:0}),['budget-pace']);
  assert.deepEqual(keys({spendBudgetUsd:40,spendSpikeMultiple:0}),['budget-over']);
  assert.deepEqual(keys({spendBudgetUsd:500,spendSpikeMultiple:5}),['spike']);
  assert.deepEqual(keys({spendBudgetUsd:500,spendSpikeMultiple:20}),[]);
  // The first day of a month counts at least one elapsed day.
  assert.equal(ui.run(`spendStatus([${JSON.stringify(row('2026-10-01',0,3))}], ${Date.UTC(2026,9,1,0,30)}).projected`),93);
});

test('spend preferences reject invalid values and keep alerts off by default',()=>{
  assert.deepEqual([settings.normalize({}).spendBudgetUsd,settings.normalize({}).spendSpikeMultiple,settings.normalize({}).spendNotify],[0,0,false]);
  const prefs=settings.normalize({spendBudgetUsd:250,spendSpikeMultiple:3,spendNotify:true});
  assert.deepEqual([prefs.spendBudgetUsd,prefs.spendSpikeMultiple,prefs.spendNotify],[250,3,true]);
  const bad=settings.normalize({spendBudgetUsd:-5,spendSpikeMultiple:1000,spendNotify:'yes'});
  assert.deepEqual([bad.spendBudgetUsd,bad.spendSpikeMultiple,bad.spendNotify],[0,0,false]);
});

test('chart CSV exports sum saved usage per period and keep formula text inert',()=>{
  const ui=dashboard();
  ui.context.rows=[row('2026-09-01',10,1),row('2026-09-02',20,2),row('2026-09-08',5,0.5),{...row('2026-09-08',1,0),costKnown:false}];
  const lines=ui.run(`groupedUsageCsv(rows,'week')`).split('\r\n');
  assert.equal(lines.length,3);
  assert.match(lines[0],/^period_start,group,input_tokens/);
  assert.equal(lines[1],'2026-08-31,week,30,0,0,0,0,30,3.000000,true,true,2');
  assert.equal(lines[2],'2026-09-07,week,6,0,0,0,0,6,0.500000,true,false,1');
  assert.equal(ui.run(`groupedUsageCsv(rows,'month')`).split('\r\n').length,2);
  assert.equal(ui.run(`csvCell('=HYPERLINK("x")')`),`"'=HYPERLINK(""x"")"`);
  assert.equal(ui.run(`csvCell('-1.5')`),'-1.5');
  assert.equal(ui.run(`csvCell(-2)`),'-2');
  const quota=ui.run(`quotaHistoryCsv([{ts:'2026-09-01T10:00:00Z',providerId:'codex',source:'oauth',primaryPercent:40,progress:[{label:'Weekly',used:40,limit:100,percent:null}]},{ts:'2026-09-01T11:00:00Z',providerId:'codex',source:'oauth',primaryPercent:55,progress:[{label:'Weekly',used:55,limit:100,percent:null}]}],'raw')`).split('\r\n');
  assert.equal(quota.length,3);
  assert.match(quota[2],/^2026-09-01T11:00:00Z,raw,codex,oauth,Weekly,55\.00,55,100,$/);
});

test('project table ranks projects by cost with shares and escapes names',()=>{
  const ui=dashboard();
  ui.context.rows=[{...row('2026-09-01',10,3),project:'api'},{...row('2026-09-05',20,1),project:'api'},{...row('2026-09-06',5,5),project:'<web>'},{...row('2026-09-06',1,0),project:''}];
  const html=ui.run('projectTable(rows)');
  const order=[...html.matchAll(/<tr><td title="([^"]*)"/g)].map(m=>m[1]);
  assert.deepEqual(JSON.parse(JSON.stringify(order)),['&lt;web&gt;','api','unknown']);
  assert.match(html,/<td>\$4\.00<\/td><td>.*?44%<\/td><td>30<\/td><td>2<\/td><td>Sep 5<\/td>/);
  assert.doesNotMatch(html,/<web>/);
});

test('hour-of-day view buckets hourly rows in local time and names the busiest hour',()=>{
  const ui=dashboard();
  ui.context.rows=[{date:'2026-09-07T09:00:00Z',totalTokens:50,cost:1},{date:'2026-09-07T09:00:00Z',totalTokens:10,cost:0},{date:'2026-09-08T22:00:00Z',totalTokens:5,cost:0}];
  const html=ui.run('hourOfDayHtml(rows)');
  const local=new Date('2026-09-07T09:00:00Z').getHours();
  assert.match(html,new RegExp(`${String(local).padStart(2,'0')}:00 · 60 tokens`));
  assert.match(html,new RegExp(`Busiest hour: ${String(local).padStart(2,'0')}:00`));
  assert.equal((html.match(/class="cal-cell l/g)||[]).length,7*24);
});

test('local analytics spans keep only rows inside the chosen UTC window',()=>{
  const ui=dashboard();
  ui.context.rows=[{date:'2026-08-30'},{date:'2026-08-31T23:00:00Z'},{date:'2026-09-06'}];
  assert.equal(ui.run(`inLocalSpan(rows,'7').length`),2);
  assert.equal(ui.run(`inLocalSpan(rows,'all').length`),3);
});

test('combined account cards group plugin instances and show each account',()=>{
  const ui=dashboard();
  ui.run(`S.allProviders=[{id:'claude',name:'Claude'},{id:'claude-work',pluginId:'claude',name:'Claude Work'},{id:'codex',name:'Codex'}];
    S.snapshots=[{providerId:'claude',displayName:'Claude',plan:'Max',fetchedAt:'2026-09-06T00:00:00Z',state:'ready',metrics:[{type:'progress',label:'Session',used:20,limit:100,format:{kind:'percent'}}]},
      {providerId:'claude-work',displayName:'Claude <Work>',fetchedAt:'2026-09-06T00:00:00Z',state:'ready',metrics:[{type:'progress',label:'Session',used:80,limit:100,format:{kind:'percent'}}]},
      {providerId:'codex',displayName:'Codex',fetchedAt:'2026-09-06T00:00:00Z',state:'ready',metrics:[]}];`);
  assert.equal(ui.run('accountGroups(S.snapshots).length'),3,'off by default');
  ui.run('S.prefs.combineAccounts=true');
  assert.equal(ui.run('accountGroups(S.snapshots).map(g=>g.length).join()'),'2,1');
  const html=ui.run('combinedCardHtml(accountGroups(S.snapshots)[0])');
  assert.match(html,/2 accounts/);
  assert.match(html,/Claude &lt;Work&gt;/);
  assert.match(html,/switchTab\('claude-work'\)/);
  assert.equal((html.match(/class="metric" data-card-component="quota"/g)||[]).length,2);
  assert.equal(settings.normalize({combineAccounts:true}).combineAccounts,true);
  assert.equal(settings.normalize({}).combineAccounts,false);
});

test('share image rows use visible providers primary quota and skip failed ones',()=>{
  const ui=dashboard([row(today,100,7)]);
  ui.run(`S.snapshots=[{providerId:'codex',displayName:'Codex',fetchedAt:'2026-09-06T00:00:00Z',state:'ready',metrics:[{type:'progress',label:'Weekly',used:42,limit:100,format:{kind:'percent'}}]},
    {providerId:'broken',displayName:'Broken',fetchedAt:'2026-09-06T00:00:00Z',state:'error',source:'error',metrics:[]}];`);
  const rows=JSON.parse(ui.run('JSON.stringify(shareSummaryRows())'));
  assert.equal(rows.length,1);
  assert.deepEqual(rows[0],{name:'Codex',label:'Weekly',percent:42,cost:7,tokens:100});
});
