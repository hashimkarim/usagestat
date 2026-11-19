const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const trends = require('../src/dashboard-trends.js');
const settings = require('../src/dashboard-settings.js');

const snapshot = {
  providerId: 'claude', displayName: 'Claude', source: 'oauth', plan: 'Max',
  fetchedAt: '2026-11-18T12:00:00Z',
  metrics: [{ type: 'progress', label: 'Session', used: 40, limit: 100, format: { kind: 'percent' } }],
};
const incident = {
  providerId: 'claude', statusPageUrl: 'https://status.claude.com/', monitored: true, state: 'ok',
  indicator: 'major', description: 'Partial Outage',
  incidents: [{ name: 'Elevated errors', status: 'investigating', impact: 'major' }],
};

function dashboard(fetchImpl) {
  const html = fs.readFileSync(require.resolve('../src/dashboard.html'), 'utf8');
  const script = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)][0][1].replace(/load\(\);\s*$/, '');
  const elements = new Map();
  const element = id => {
    if (!elements.has(id)) elements.set(id, { id, style: {}, dataset: {}, innerHTML: '',
      classList: { add() {}, remove() {}, toggle() {} },
      querySelector: selector => element(selector), querySelectorAll: () => [], contains: () => false,
      addEventListener: (event, fn) => { element(id)[event] = fn; } });
    return elements.get(id);
  };
  const context = vm.createContext({ UsageDashboardSettings: settings, UsageTrends: trends,
    URL, fetch: fetchImpl, setInterval: () => 0, clearInterval: () => {}, matchMedia: () => ({ matches: true }),
    localStorage: { getItem: () => null, setItem: () => {} },
    window: { addEventListener: () => {} },
    document: { documentElement: { dataset: {}, style: { setProperty() {} } }, getElementById: element,
      querySelectorAll: () => [] } });
  vm.runInContext(script, context);
  const run = code => vm.runInContext(code, context);
  return { context, element, run, badge: row => {
    context.row = row;
    return run('S.status=row===undefined?null:[row];incidentBadgeHtml("claude")');
  } };
}

test('no badge when operational, unmonitored, unknown or missing', () => {
  const ui = dashboard();
  assert.equal(ui.badge(undefined), '');
  assert.equal(ui.badge({ ...incident, indicator: 'none' }), '');
  assert.equal(ui.badge({ ...incident, monitored: false }), '');
  assert.equal(ui.badge({ ...incident, indicator: undefined, state: 'unavailable' }), '');
  assert.equal(ui.badge({ ...incident, providerId: 'codex' }), '');
  // An unexpected indicator never reaches a class name or the label.
  assert.equal(ui.badge({ ...incident, indicator: 'x" onmouseover="alert(1)' }), '');
  assert.equal(ui.badge({ ...incident, indicator: 'constructor' }), '');
});

test('badge links to the status page and summarizes active incidents', () => {
  const html = dashboard().badge(incident);
  assert.match(html, /^<a class="badge b-incident b-incident-major" href="https:\/\/status\.claude\.com\/"/);
  assert.match(html, /target="_blank" rel="noreferrer"/);
  assert.match(html, /title="Partial Outage · Elevated errors"/);
  assert.match(html, />Major outage<\/a>$/);
  const stale = dashboard().badge({ ...incident, indicator: 'maintenance', state: 'stale', incidents: [] });
  assert.match(stale, /b-incident-maintenance/);
  assert.match(stale, /title="Partial Outage \(last known status\)"/);
});

test('status text is escaped and unsafe links are dropped', () => {
  const html = dashboard().badge({ ...incident, indicator: 'critical',
    statusPageUrl: 'javascript:alert(1)',
    description: '"><img src=x onerror=alert(1)>',
    incidents: [{ name: '<script>alert(2)</script>' }, null, { name: 7 }] });
  assert.ok(html.startsWith('<span class="badge b-incident b-incident-critical"'));
  assert.ok(!html.includes('href=') && !html.includes('javascript:'));
  assert.ok(!html.includes('<img') && !html.includes('<script'));
  assert.match(html, /&quot;&gt;&lt;img src=x onerror=alert\(1\)&gt; · &lt;script&gt;alert\(2\)&lt;\/script&gt;/);
});

test('overview cards and the provider tab render the badge from /v1/status', async () => {
  const urls = [];
  const ui = dashboard(async url => {
    urls.push(url);
    const body = url === '/v1/usage' ? [snapshot]
      : url === '/v1/providers' ? [{ id: 'claude', name: 'Claude', enabled: true }]
      : url === '/v1/status' ? [incident] : null;
    return { ok: body !== null, json: async () => body };
  });
  ui.run('buildTabs=()=>{};markActiveTab=()=>{};applyTabVisibility=()=>{};renderActive=()=>{};checkThresholdNotifications=()=>{};checkSpendNotifications=()=>{};setOnline=()=>{};updAgo=()=>{};startTick=()=>{};');
  await ui.run('load()');
  assert.ok(urls.includes('/v1/status'));
  ui.run('renderOverview()');
  assert.match(ui.element('grid').innerHTML, /b-incident-major[^>]*>Major outage/);
  ui.element('panel-claude');
  // The header renders before analytics arrive; keep analytics pending so only it is checked.
  ui.run('fetchHistory=fetchCost=fetchCcusageReports=ensureModelReports=()=>new Promise(()=>{});renderProvider("claude")');
  assert.match(ui.element('panel-claude').innerHTML, /dh-tags">.*b-incident-major/s);
});

test('an older daemon without the route keeps the dashboard working', async () => {
  const ui = dashboard(async url => {
    if (url === '/v1/status') throw new Error('offline');
    const body = url === '/v1/usage' ? [snapshot] : url === '/v1/providers' ? [{ id: 'claude', name: 'Claude', enabled: true }] : null;
    return { ok: body !== null, json: async () => body };
  });
  ui.run('buildTabs=()=>{};markActiveTab=()=>{};applyTabVisibility=()=>{};renderActive=()=>{};checkThresholdNotifications=()=>{};checkSpendNotifications=()=>{};setOnline=v=>{globalThis.online=v};updAgo=()=>{};startTick=()=>{};');
  await ui.run('load()');
  assert.equal(ui.run('S.online'), true);
  assert.equal(ui.run('S.status'), null);
  ui.run('renderOverview()');
  assert.ok(!ui.element('grid').innerHTML.includes('b-incident'));
});

test('project table expands a project into its git branches', () => {
  const ui = dashboard();
  ui.context.rows = [
    { date: '2026-11-18', project: 'app', branch: 'feat/x', costUsd: 3, totalTokens: 30, models: {} },
    { date: '2026-11-17', project: 'app', branch: '', costUsd: 1, totalTokens: 10, models: {} },
    { date: '2026-11-18', project: 'site', costUsd: 0.5, totalTokens: 5, models: {} },
  ];
  const html = ui.run('projectTable(rows)');
  assert.match(html, /<tr class="project-row"[^>]*data-project="0"><td title="app">/);
  assert.match(html, /<tr class="branch-row" data-parent="0" hidden><td title="feat\/x">feat\/x<\/td><td>\$3\.00<\/td><td>75% of project/);
  assert.match(html, /<td title="no branch recorded">\(no branch\)/);
  // A project with no recorded branch is not expandable.
  assert.match(html, /<tr data-project="1"><td title="site">site/);
});
