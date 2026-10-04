#!/usr/bin/env node
// Reproduce reviewed bundled fetchers, keeping upstream parsing and its license intact.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {createHash} = require('node:crypto');
const {execFileSync} = require('node:child_process');
const root = path.resolve(__dirname, '..');
const upstream = path.join(root, 'inspo/CodexBar');
const revision = '6ba06fe9ec1522dd5c59e16810115bce28aa148f';
const catalog = {
  aixy: ['Budget', 'Secondary budget', 'https://dash.aixy-gateway.com', {AIXY_BASE_URL: 'https://api.aixy-gateway.com'}],
  atlascloud: ['Balance', 'Balance', 'https://www.atlascloud.ai/console'],
  bifrost: ['Budget', 'Secondary budget', null],
  devpass: ['Plan credits', 'Premium weekly', 'https://devpass.llmgateway.io/dashboard'],
  gitkraken: ['Personal', 'Shared pool', 'https://gitkraken.dev/account#ai-usage'],
  helmcode: ['Model quota', 'Model quota', 'https://cloud.helmcode.com/dashboard'],
  huggingface: ['Inference', 'ZeroGPU', 'https://huggingface.co/settings/billing'],
  hyper: ['Balance', 'Balance', 'https://hyper.charm.land'],
  llmman: ['Memory', 'Models', null, {LLMMAN_HOST: 'http://127.0.0.1:17434'}],
  muse: ['Session', 'Weekly', 'https://dev.meta.ai'],
  museai: ['Weekly', 'Additional tokens', 'https://muse.ai'],
  lithosai: ['Balance', 'Spend', 'https://console.lithosai.cloud'],
  workbuddy: ['Credits', 'Credits', 'https://www.workbuddy.cn'],
  nous: ['Monthly credits', 'Weekly', 'https://portal.nousresearch.com/usage', {PORTAL_URL: 'https://portal.nousresearch.com'}],
  raycast: ['Credits', 'Plan', 'https://www.raycast.com/settings'],
  replicate: ['Spend', 'Spend', 'https://replicate.com/account/billing'],
  typesafe: ['Spend', 'Spend', 'https://console.typesafe.ai/usage'],
  v0: ['Billing', 'Rate limit', 'https://v0.app/settings/billing'],
  vercel: ['Balance', 'Balance', 'https://vercel.com/d?to=%2F%5Bteam%5D%2F%7E%2Fai-gateway'],
  xkiro: ['Daily free tokens', 'Weekly', 'https://xkiro.com'],
};
if (execFileSync('git', ['rev-parse', 'HEAD'], {cwd: upstream, encoding: 'utf8'}).trim() !== revision)
  throw new Error('Review and pin the upstream revision before regenerating providers.');
const check = process.argv.includes('--check');
function write(file, content) {
  if (check) {
    if (!fs.existsSync(file) || fs.readFileSync(file, 'utf8') !== content) throw new Error(`Out of date: ${file}`);
  } else {
    fs.mkdirSync(path.dirname(file), {recursive: true});
    fs.writeFileSync(file, content);
  }
}
for (const [id, [primary, secondary, dashboard, defaults = {}]] of Object.entries(catalog)) {
  const sourcePath = `Sources/CodexBarCore/Resources/Plugins/${id}.js`;
  const source = fs.readFileSync(path.join(upstream, sourcePath), 'utf8');
  let definition;
  vm.runInNewContext(source, {defineProvider: value => { definition = value; }}, {timeout: 1000});
  if (definition.id !== id) throw new Error(`Unexpected provider: ${id}`);
  const webOnly = ['helmcode', 'raycast', 'replicate', 'typesafe', 'museai', 'lithosai', 'workbuddy'].includes(id);
  const options = {labels: [primary, secondary, 'Additional'], defaults, autoMode: webOnly ? 'web' : 'api',
    ...(id === 'museai' ? {maxRequests:160} : {}), ...(id === 'lithosai' ? {balanceOnly:true} : {})};
  const modes = webOnly ? ['web'] : id === 'hyper' ? ['api', 'web'] : ['api'];
  const envVars = definition.settings.map(setting => setting.key).filter(key => /^[A-Z][A-Z0-9_]+$/.test(key) && !['CLIENT_VERSION', 'SOURCE_MODE', 'TENANT'].includes(key));
  if (definition.cookieDomains?.length) envVars.push(`${id.toUpperCase()}_COOKIE`);
  const manifest = {
    schemaVersion: 1, id, name: definition.name, version: '0.1.0', entry: 'plugin.js',
    enabledByDefault: false, supportedModes: modes, autoMode: webOnly ? 'web' : 'api',
    ...(dashboard ? {usageDashboardUrl: dashboard} : {}),
    ...(definition.cookieDomains?.length ? {webUrl: `https://${definition.cookieDomains[0]}`} : {}),
    envVars,
    ...({
      museai: {setupHelp:'Use a session Cookie header from muse.ai. This provider is separate from Muse Code and does not use its CLI login.'},
      lithosai: {setupHelp:'Use a Cookie header from console.lithosai.cloud containing both __Host-console_session and __Host-console_csrf from the same session. Prepaid balance is separate from measured spend.'},
      workbuddy: {setupHelp:'Use a Cookie header from www.workbuddy.cn and the full User-Agent from the same browser. Set Browser User-Agent below. Desktop login files are not imported.'},
    }[id] || {}),
    ...(id === 'workbuddy' ? {setupFields:[{key:'browserUserAgent',title:'Browser User-Agent',type:'string',
      description:'Paste the full User-Agent from the same browser session that supplied the cookies. Required for WorkBuddy web readback.'}]} : {}),
    upstream: {repository: 'https://github.com/steipete/CodexBar', revision, path: sourcePath,
      sha256: createHash('sha256').update(source).digest('hex'), license: 'MIT'},
  };
  const dest = path.join(root, 'plugins', id);
  write(path.join(dest, 'plugin.json'), JSON.stringify(manifest, null, 2) + '\n');
  write(path.join(dest, 'plugin.js'), '// Generated by tools/vendor-codexbar-providers.cjs; see LICENSE.\n' +
    `globalThis.__usagestat_bundled_options = ${JSON.stringify(options)};\n` + source);
  write(path.join(dest, 'LICENSE'), fs.readFileSync(path.join(upstream, 'LICENSE'), 'utf8'));
  if (['museai','lithosai','workbuddy'].includes(id))
    write(path.join(dest,'icon.svg'), fs.readFileSync(path.join(upstream,'docs/logos',id+'.svg'),'utf8').replace(/fill="white"/g,'fill="currentColor"'));
}
console.log(`${check ? 'Checked' : 'Vendored'} ${Object.keys(catalog).length} providers at ${revision}`);
