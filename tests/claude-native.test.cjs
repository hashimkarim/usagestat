const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');

const root = path.resolve(__dirname, '..');
const binary = process.env.USAGESTAT_TEST_CLI || path.join(root, 'target/debug/usagestat');

function fixture(t, mode) {
  assert.ok(fs.existsSync(binary), 'Build usagestat-cli before running native provider tests');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'usagestat-claude-test-'));
  t.after(() => fs.rmSync(dir, {recursive: true, force: true}));
  const profile = path.join(dir, 'claude');
  const helpers = path.join(dir, 'bin');
  fs.mkdirSync(profile); fs.mkdirSync(helpers);
  fs.writeFileSync(path.join(profile, '.credentials.json'), JSON.stringify({claudeAiOauth: {accessToken: 'fake-native-test-token', scopes: ['user:profile']}}));
  const quota = {five_hour: {utilization: 19, resets_at: null}, seven_day: {utilization: 2, resets_at: new Date(Date.now() + 86400000).toISOString()}};
  const config = {oauthAccount: {accountUuid: 'account', organizationUuid: 'org', organizationName: 'Test organization', emailAddress: 'test@example.test'}};
  const at = Date.now() - 30000;
  if (mode === 'cached') config.cachedUsageUtilization = {accountUuid: 'account', fetchedAtMs: at, utilization: quota};
  fs.writeFileSync(path.join(profile, '.claude.json'), JSON.stringify(config));
  fs.writeFileSync(path.join(dir, 'config.toml'), '[[providers]]\nid = "claude"\nenabled = true\nsource = "auto"\n');
  const helper = `#!${process.execPath}
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const args=process.argv.slice(2);
assert.ok(args.includes('--no-session-persistence'));
assert.ok(args.includes('--strict-mcp-config'));
assert.ok(args.includes('--setting-sources='));
assert.equal(args[args.indexOf('--tools')+1],'');
assert.deepEqual(JSON.parse(args[args.indexOf('--settings')+1]),{disableAllHooks:true});
assert.deepEqual(JSON.parse(args[args.indexOf('--mcp-config')+1]),{mcpServers:{}});
const profile=process.env.CLAUDE_CONFIG_DIR;
const calls=[]; let quota=null;
const send=(request,body)=>process.stdout.write(JSON.stringify({type:'control_response',response:{request_id:request.request_id,subtype:'success',response:body}})+'\\n');
require('node:readline').createInterface({input:process.stdin}).on('line',line=>{
 const request=JSON.parse(line); assert.equal(request.type,'control_request');
 calls.push(request.request.subtype);
 fs.writeFileSync(path.join(profile,'calls.json'),JSON.stringify(calls));
 if(request.request.subtype==='initialize') send(request,{account:{email:'test@example.test',organization:'Test organization',apiProvider:'firstParty'}});
 else { assert.equal(request.request.subtype,'get_usage'); assert.equal(request.request.skip_behaviors,true);
   quota=${mode === 'empty' ? 'null' : JSON.stringify(quota)};
   send(request,{rate_limits_available:true,rate_limits:quota});
 }
}).on('close',()=>{
 // Persist only at graceful EOF, exposing premature process termination.
 if(quota){const file=path.join(profile,'.claude.json'),config=JSON.parse(fs.readFileSync(file));
 config.cachedUsageUtilization={accountUuid:'account',fetchedAtMs:Date.now(),utilization:quota};fs.writeFileSync(file,JSON.stringify(config));}
});
`;
  fs.writeFileSync(path.join(helpers, 'claude'), helper, {mode: 0o700});
  const env = {...process.env, HOME: dir, CLAUDE_CONFIG_DIR: profile, USAGESTAT_DATA_DIR: path.join(dir, 'data'),
    USAGESTAT_CONFIG_DIR: path.join(dir, 'config'), XDG_CONFIG_HOME: path.join(dir, 'config'),
    USAGESTAT_HELPER_PATH: helpers};
  for (const key of Object.keys(env)) if (/^(ANTHROPIC_|CLAUDE_CODE_|CLAUDE_SECURESTORAGE_|USE_LOCAL_OAUTH|USE_STAGING_OAUTH)/.test(key)) delete env[key];
  const probe = source => {
    const result = spawnSync(binary, ['--config', path.join(dir, 'config.toml'), '--plugin-dir', path.join(root, 'plugins'), 'usage', 'claude', '--source', source, '--json'],
      {env, cwd: dir, encoding: 'utf8', timeout: 20000, maxBuffer: 2 * 1024 * 1024});
    assert.equal(result.status, 0, result.stderr || String(result.error));
    assert.ok(result.stdout.trim(), result.stderr || 'missing CLI output');
    return JSON.parse(result.stdout)[0];
  };
  return {probe, profile, at};
}

test('native Claude auto reads the selected profile cache without launching a helper', {skip: process.platform === 'win32'}, t => {
  const f = fixture(t, 'cached'), result = f.probe('auto');
  assert.equal(result.state, 'ready'); assert.equal(result.source, 'cli-cache');
  assert.equal(Date.parse(result.fetchedAt), f.at);
  assert.equal(result.metrics.find(metric => metric.label === 'Session').used, 19);
  assert.equal(result.metrics.find(metric => metric.label === 'Weekly').used, 2);
  assert.ok(!fs.existsSync(path.join(f.profile, 'calls.json')));
});

test('native Claude standalone dialogue sends no prompt and allows graceful cache persistence', {skip: process.platform === 'win32'}, t => {
  const f = fixture(t, 'fresh'), result = f.probe('cli');
  assert.equal(result.state, 'ready'); assert.equal(result.source, 'cli');
  assert.equal(result.metrics.find(metric => metric.label === 'Session').used, 19);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(f.profile, 'calls.json'))), ['initialize', 'get_usage']);
  const config = JSON.parse(fs.readFileSync(path.join(f.profile, '.claude.json')));
  assert.equal(Date.parse(result.fetchedAt), config.cachedUsageUtilization.fetchedAtMs);
  assert.equal(f.probe('cli').state, 'ready');
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(f.profile, 'calls.json'))), ['initialize', 'get_usage']);
});

test('native Claude null quota is unavailable, never a successful zero reading', {skip: process.platform === 'win32'}, t => {
  const f = fixture(t, 'empty'), result = f.probe('cli');
  assert.equal(result.state, 'failed');
  assert.ok(!result.metrics.some(metric => metric.type === 'progress'));
});
