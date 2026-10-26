#!/usr/bin/env node
// Reproduce reviewed bundled fetchers, keeping upstream parsing and its license intact.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {createHash} = require('node:crypto');
const {execFileSync} = require('node:child_process');
const root = path.resolve(__dirname, '..');
const upstream = path.join(root, 'inspo/CodexBar');
const revision = '6070b29a1a5d51193a634dc1b0a14a67c39d0ccc';
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
  abliteration: ['Credits', 'Additional', 'https://abliteration.ai'],
  aerostack: ['Monthly AI tokens', 'Additional', 'https://app.aerostack.dev'],
  chatbox: ['Plan', 'Top-up credits', 'https://chatboxai.app'],
  claudeplatform: ['Credits', 'Additional', 'https://platform.claude.com/settings/billing'],
  cosmic: ['Input tokens', 'Output tokens', 'https://app.cosmicjs.com'],
  exa: ['Usage', 'Additional', 'https://dashboard.exa.ai'],
  fal: ['Balance', 'Additional', 'https://fal.ai/dashboard/billing'],
  flora: ['Credits', 'Additional', 'https://app.flora.ai'],
  futureagi: ['AI credits', 'Gateway requests', 'https://app.futureagi.com'],
  langdock: ['Session', 'Weekly', 'https://app.langdock.com/settings/account/usage'],
  linkup: ['Balance', 'Additional', 'https://app.linkup.so'],
  munito: ['Daily spend limit', 'Monthly spend limit', 'https://app.munito.ai'],
  parallel: ['Balance', 'Additional', 'https://platform.parallel.ai'],
  sailresearch: ['Balance', 'Additional', 'https://sailresearch.com'],
  sofya: ['Credits', 'Additional', 'https://sofya.co'],
  soniox: ['Monthly budget', 'Additional', 'https://console.soniox.com'],
  tavily: ['Account plan', 'API key', 'https://app.tavily.com'],
  tinyapi: ['Credits', 'Additional', 'https://tinyapi.rest'],
  traecn: ['Credits', 'Additional', 'https://www.trae.cn'],
  xapi: ['Balance', 'Additional', 'https://console.x.com'],
  'gemini-apps': ['Current', 'Weekly', 'https://gemini.google.com/usage'],
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
function readDefinition(name, id = name) {
  const sourcePath = `Sources/CodexBarCore/Resources/Plugins/${name}.js`;
  const source = fs.readFileSync(path.join(upstream, sourcePath), 'utf8');
  let definition;
  vm.runInNewContext(source, {defineProvider: value => {definition = value;}}, {timeout:1000});
  if (definition.id !== id) throw new Error(`Unexpected provider: ${name}`);
  return {source,definition,provenance:{repository:'https://github.com/steipete/CodexBar',revision,path:sourcePath,
    sha256:createHash('sha256').update(source).digest('hex'),license:'MIT'}};
}
function wrapped(input, callback) {
  return `(function(defineProvider) {\n// BEGIN UPSTREAM ${input.provenance.path}\n${input.source}\n// END UPSTREAM\n})(${callback});\n`;
}
function dispatch(id, setting, primary, alternate) {
  return `globalThis.__usagestat_plugin = {id:${JSON.stringify(id)},probe(ctx) {\n` +
    `const selected=ctx.provider?.settings?.${setting};\n` +
    `if(selected!==undefined && typeof selected!=='boolean') throw new Error('${setting} must be a boolean.');\n` +
    `return (selected===true ? ${alternate} : ${primary}).probe(ctx);\n}};\n`;
}
for (const [id, [primary, secondary, dashboard, defaults = {}]] of Object.entries(catalog)) {
  const input = readDefinition(id === 'gemini-apps' ? 'gemini-web' : id, id === 'gemini-apps' ? 'gemini' : id);
  const {definition} = input;
  const webOnly = !definition.auth && !!definition.cookieDomains?.length && id !== 'hyper' && id !== 'muse';
  const options = {labels: [primary, secondary, 'Additional'], defaults, autoMode: webOnly ? 'web' : 'api',
    ...(id === 'gemini-apps' ? {providerId:id,displayName:'Gemini Apps',cookieEnv:'GEMINI_COOKIE'} : {}),
    ...(id === 'museai' ? {maxRequests:160} : {}), ...(['lithosai','xapi','parallel','claudeplatform'].includes(id) ? {balanceOnly:true} : {})};
  const modes = webOnly ? ['web'] : id === 'hyper' ? ['api', 'web'] : ['api'];
  const envVars = definition.settings.map(setting => setting.key).filter(key => /^[A-Z][A-Z0-9_]+$/.test(key) && !['CLIENT_VERSION', 'SOURCE_MODE', 'TENANT', 'MESSAGES'].includes(key));
  if (definition.cookieDomains?.length) envVars.push(options.cookieEnv || `${id.toUpperCase()}_COOKIE`);
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
    setupFields: [
      ...definition.settings.filter(setting => !['MESSAGES','CLIENT_VERSION','SOURCE_MODE'].includes(setting.key) &&
        setting.key !== definition.settings.find(candidate => candidate.type === 'secure')?.key)
        .map(setting => ({key:setting.key,title:setting.title,type:'string',
          description:setting.type === 'secure' ? 'Additional credential for this provider instance; stored privately.' : 'Provider-specific account or source selection.'})),
      ...(id === 'workbuddy' ? [{key:'browserUserAgent',title:'Browser User-Agent',type:'string',
        description:'Paste the full User-Agent from the same browser session that supplied the cookies. Required for WorkBuddy web readback.'}] : []),
    ],
    upstream: input.provenance,
  };
  const dest = path.join(root, 'plugins', id);
  let entry = '// Generated by tools/vendor-codexbar-providers.cjs; see LICENSE.\n' +
    `globalThis.__usagestat_bundled_options = ${JSON.stringify(options)};\n` + wrapped(input,'globalThis.defineProvider');
  if (id === 'vercel') {
    const budget = readDefinition('vercel-budget','vercel');
    const budgetOptions = {labels:['Personal budget','Additional','Additional'],autoMode:'api',genericApiKey:false};
    manifest.upstreamSources = [budget.provenance];
    manifest.envVars.push('VERCEL_ACCESS_TOKEN','VERCEL_TEAM_ID');
    manifest.setupFields.push({key:'personalBudget',title:'Read personal budget',type:'boolean',description:'Use a Vercel access token and Team ID instead of AI Gateway key credits.'},
      {key:'VERCEL_ACCESS_TOKEN',title:'Vercel access token',type:'string',description:'Separate account token; the AI Gateway API key is not used for budgets.'},
      {key:'VERCEL_TEAM_ID',title:'Team ID',type:'string',description:'Team ID or slug whose personal budget belongs to the token user.'});
    entry += `(function() { const credits=globalThis.__usagestat_plugin; let budget;\n` +
      wrapped(budget,`definition => { budget=globalThis.__usagestat_bundledProvider(definition,${JSON.stringify(budgetOptions)}); }`) +
      dispatch(id,'personalBudget','credits','budget') + '})();\n';
  }
  write(path.join(dest, 'plugin.json'), JSON.stringify(manifest, null, 2) + '\n');
  write(path.join(dest, 'plugin.js'), entry);
  write(path.join(dest, 'LICENSE'), fs.readFileSync(path.join(upstream, 'LICENSE'), 'utf8'));
  if (['museai','lithosai','workbuddy'].includes(id) ||
      !fs.existsSync(path.join(dest,'icon.svg')) && fs.existsSync(path.join(upstream,'docs/logos',id+'.svg')))
    write(path.join(dest,'icon.svg'), fs.readFileSync(path.join(upstream,'docs/logos',id+'.svg'),'utf8').replace(/fill="white"/g,'fill="currentColor"'));
}
const team = readDefinition('qwencloud-team','qwencloud');
const qwenDir=path.join(root,'plugins/qwencloud');
const qwenManifest=JSON.parse(fs.readFileSync(path.join(qwenDir,'plugin.json'),'utf8'));
qwenManifest.upstreamSources=[team.provenance];
qwenManifest.setupFields=[{key:'teamTokenPlan',title:'Team Token Plan',type:'boolean',description:'Read the Team subscription for this cookie; otherwise read the Individual subscription. No fallback between scopes.'}];
write(path.join(qwenDir,'plugin.json'),JSON.stringify(qwenManifest,null,2)+'\n');
write(path.join(qwenDir,'plugin.js'),'// Generated by tools/vendor-codexbar-providers.cjs from individual.js and reviewed upstream; see LICENSE.\n' +
  '(function() { let team;\n' + wrapped(team,"definition => { team=globalThis.__usagestat_bundledProvider(definition,{labels:['Team credits','Additional','Additional'],autoMode:'web'}); }") +
  fs.readFileSync(path.join(qwenDir,'individual.js'),'utf8') + '\nconst individual=globalThis.__openusage_plugin;\n' +
  dispatch('qwencloud','teamTokenPlan','individual','team') + '})();\n');
write(path.join(qwenDir,'LICENSE'),fs.readFileSync(path.join(upstream,'LICENSE'),'utf8'));
console.log(`${check ? 'Checked' : 'Vendored'} ${Object.keys(catalog).length} providers and Qwen Team source at ${revision}`);
