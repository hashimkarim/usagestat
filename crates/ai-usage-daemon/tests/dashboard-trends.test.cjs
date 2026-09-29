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
      querySelector: selector => element(selector),
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
  assert.equal(ui.run('barCol(90)'),'#ce8670');
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

test('saved model mix clears stale output after an empty or failed refresh',async()=>{
  for(const ok of [true,false]){
    const ui=dashboard();let finish;
    ui.context.fetch=()=>new Promise(resolve=>{finish=resolve;});
    ui.run(`S.modelReports={codex:{status:'ok',at:0,rows:[{date:'${today}',models:{old:{costUsd:1,costKnown:true}}}]}}`);
    ui.element('model-mix-codex').innerHTML=ui.run("savedModelMix('codex')");
    assert.match(ui.element('model-mix-codex').innerHTML,/old/);
    finish({ok,json:async()=>({daily:[]})});
    await new Promise(resolve=>setImmediate(resolve));
    assert.doesNotMatch(ui.element('model-mix-codex').innerHTML,/model-label">old/);
    assert.match(ui.element('model-mix-codex').innerHTML,ok?/No saved model history/:/Could not load saved model history/);
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

test('timeline gradient identifiers cannot inject markup and series labels are escaped',()=>{
  const ui=dashboard();
  const svg={querySelector:()=>({children:[]}),addEventListener:()=>{}};
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
  ui.context.chart={id:'c',clientWidth:860,innerHTML:'',querySelector:()=>({addEventListener(){},querySelector:()=>({children:[],setAttribute(){}}),getBoundingClientRect:()=>({left:0,width:860})})};
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
