const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const dashboard = JSON.parse(fs.readFileSync(path.join(root, 'docs/grafana/usagestat-dashboard.json'), 'utf8'));
const exporter = fs.readFileSync(path.join(root, 'crates/usagestat-daemon/src/prometheus.rs'), 'utf8');

// Metric families the exporter declares with "# TYPE <name> gauge".
const exported = new Set([...exporter.matchAll(/# TYPE (usagestat_\w+) /g)].map(match => match[1]));
const exprs = dashboard.panels.flatMap(panel => panel.targets.map(target => ({ source: panel.title, expr: target.expr })));
const variables = dashboard.templating.list.filter(item => item.type === 'query')
  .map(item => ({ source: `$${item.name}`, expr: item.query.query }));

test('the exporter declares the metrics the dashboard is built on', () => {
  assert.deepEqual([...exported].sort(), [
    'usagestat_fetched_timestamp_seconds', 'usagestat_provider_up',
    'usagestat_reset_timestamp_seconds', 'usagestat_usage_ratio',
  ]);
});

test('every queried metric exists in the exporter and every metric is charted', () => {
  const queried = new Set();
  for (const { source, expr } of [...exprs, ...variables]) {
    const names = [...expr.matchAll(/\b(usagestat_\w+)/g)].map(match => match[1]);
    assert.ok(names.length > 0, `${source} queries no usagestat metric`);
    for (const name of names) {
      assert.ok(exported.has(name), `${source} queries unknown metric ${name}`);
      queried.add(name);
    }
  }
  assert.deepEqual([...queried].sort(), [...exported].sort());
});

test('panels filter only on exported labels and honour the dashboard variables', () => {
  const allowed = new Set(['provider', 'window', 'instance']);
  for (const { source, expr } of exprs) {
    for (const match of expr.matchAll(/(\w+)=~?"/g)) assert.ok(allowed.has(match[1]), `${source} filters on ${match[1]}`);
    assert.match(expr, /instance=~"\$instance",provider=~"\$provider"/, `${source} ignores the dashboard filters`);
  }
});

test('dashboard is provisioning-ready', () => {
  assert.equal(dashboard.uid, 'usagestat-overview');
  assert.equal(dashboard.id, null);
  const ids = dashboard.panels.map(panel => panel.id);
  assert.equal(new Set(ids).size, ids.length, 'panel ids are unique');
  assert.ok(dashboard.templating.list.some(item => item.type === 'datasource' && item.query === 'prometheus'));
  for (const panel of dashboard.panels) {
    assert.deepEqual(panel.datasource, { type: 'prometheus', uid: '${datasource}' }, panel.title);
    for (const target of panel.targets) assert.deepEqual(target.datasource, panel.datasource, panel.title);
  }
  const provider = fs.readFileSync(path.join(root, 'docs/grafana/provisioning/usagestat-dashboards.yaml'), 'utf8');
  assert.match(provider, /^apiVersion: 1$/m);
  assert.match(provider, /path: \/var\/lib\/grafana\/dashboards\/usagestat$/m);
});
