const {test} = require('node:test');
const assert = require('node:assert/strict');
const {providerHarness} = require('./provider-harness.cjs');
const json = body => ({status: 200, bodyText: JSON.stringify(body)});
const quota = {userStatus: {planStatus: {dailyQuotaRemainingPercent: 80, weeklyQuotaRemainingPercent: 60, planInfo: {planName: 'Fixture'}}}};
const models = {models: {fixture: {displayName: 'Gemini Pro', quotaInfo: {remainingFraction: 0.75}}}};
function protoField(id, value) {
  const data = Buffer.isBuffer(value) ? value : Buffer.from(value);
  assert(data.length < 128);
  return Buffer.concat([Buffer.from([id * 8 + 2, data.length]), data]);
}
function oauthRow(token = 'synthetic-access', refresh = 'synthetic-refresh') {
  const inner = Buffer.concat([protoField(1, token), protoField(3, refresh)]).toString('base64');
  return [{value: protoField(1, Buffer.concat([protoField(1, 'oauthTokenInfoSentinelKey'), protoField(2, protoField(1, inner))])).toString('base64')}];
}
for (const platform of ['linux', 'macos', 'windows']) {
  test(`${platform}: Devin native CLI roots, explicit files and IDE accounts are isolated`, () => {
    const h = providerHarness('devin', {platform, http: () => json(quota)});
    const credentials = platform === 'windows' ? h.ctx.host.fs.localAppDataPath('devin/credentials.toml') : '~/.local/share/devin/credentials.toml';
    h.files.set(h.normalize(credentials), 'windsurf_api_key = "synthetic-cli"');
    assert.deepEqual(Array.from(h.probe().lines, x => x.used), [20, 40]);
    h.databases.set(h.normalize(h.ctx.host.fs.appSupportPath('Devin/User/globalStorage/state.vscdb')), [{value: '{"apiKey":"synthetic-ide"}'}]);
    const before = h.calls.http.length;
    assert.throws(() => h.probe(), e => /Multiple Devin accounts/.test(e.message));
    assert.equal(h.calls.http.length, before);
    h.ctx.provider.settings = {ideVariant: 'devin'};
    assert.deepEqual(Array.from(h.probe().lines, x => x.used), [20, 40]);
    assert.equal(JSON.parse(h.calls.http.at(-1).bodyText).metadata.apiKey, 'synthetic-ide');
    h.ctx.provider.settings = {credentialsPath: h.home + '/missing 使用.toml'};
    assert.throws(() => h.probe(), e => /login/.test(String(e)));
    assert.equal(h.calls.http.length, before + 1);
    // A malformed selected file cannot fall through to the other IDE account.
    h.files.set(h.normalize(h.ctx.provider.settings.credentialsPath), 'garbage');
    assert.throws(() => h.probe(), e => e.code === 'credential-malformed');
    h.ctx.provider.settings = {authSource: 'cli'};
    h.ctx.host.http.request = request => { h.calls.http.push(request); return {status: 401, bodyText: '{}'}; };
    const queries = h.calls.sqlite.length;
    assert.throws(() => h.probe(), e => /login/.test(String(e)));
    assert.equal(h.calls.sqlite.length, queries);
    if (platform === 'windows') assert(!h.calls.files.some(p => p.includes('.local')));
  });

  test(`${platform}: Antigravity selects native database and rejects other profiles and stale cache`, () => {
    const h = providerHarness('antigravity', {platform, settings: {ideVariant: 'antigravity'}, http: () => json(models),
      host: {ls: {discoverStatus() { throw new Error('Explicit database unexpectedly used process discovery'); }}}});
    h.ctx.app.pluginDataDir = h.home + '/isolated plugin state';
    const standard = h.ctx.host.fs.appSupportPath('Antigravity/User/globalStorage/state.vscdb');
    const ide = h.ctx.host.fs.appSupportPath('Antigravity IDE/User/globalStorage/state.vscdb');
    h.databases.set(h.normalize(standard), oauthRow());
    h.databases.set(h.normalize(ide), oauthRow('other-account', 'other-refresh'));
    h.files.set(h.normalize(h.ctx.app.pluginDataDir + '/auth.json'), JSON.stringify({accessToken: 'stale-other-account', expiresAtMs: Date.now() + 3600000}));
    assert.equal(h.probe().lines[0].used, 25);
    assert(h.calls.http.every(request => request.headers.Authorization === 'Bearer synthetic-access'));
    assert.equal(h.calls.sqlite.length, 1);
    // No selected profile: both native databases are found and ambiguity is explicit.
    h.ctx.provider.settings = {};
    h.ctx.host.ls = {discoverStatus: () => ({status: 'missing'})};
    const before = h.calls.http.length;
    assert.throws(() => h.probe(), e => /Multiple Antigravity credential/.test(e.message));
    assert.equal(h.calls.http.length, before);
    h.ctx.provider.settings = {userDataDir: h.home + '/missing profile'};
    assert.throws(() => h.probe(), e => /Start Antigravity/.test(String(e)));
    assert.equal(h.calls.http.length, before);
    // A selected account's failed auth cannot use the other profile's cache/keychain.
    h.ctx.provider.settings = {ideVariant: 'antigravity'};
    h.ctx.host.keychain = {readGenericPassword() { throw new Error('Other account queried'); }};
    h.ctx.host.http.request = request => {h.calls.http.push(request); return {status: 401, bodyText: '{}'};};
    assert.throws(() => h.probe(), e => /Start Antigravity/.test(String(e)));
    assert(!h.calls.http.slice(before).some(request => request.headers.Authorization === 'Bearer stale-other-account'));
    h.ctx.provider.settings = {};
    h.ctx.host.ls = {discoverStatus: () => ({status: 'ambiguous'})};
    assert.throws(() => h.probe(), e => /Multiple Antigravity processes/.test(e.message));
  });

  test(`${platform}: Antigravity falls back to agy login when the IDE database is stale`, () => {
    // agy's keyring payload: expired access token nested under token, plus a top-level ID token.
    const agy = JSON.stringify({token: {access_token: 'agy-expired', refresh_token: 'agy-refresh', expiry: '2020-01-01T00:00:00Z'}, id_token: 'agy-id-token'});
    const h = providerHarness('antigravity', {platform, http: request => {
      const auth = request.headers && request.headers.Authorization;
      if (/oauth2\.googleapis\.com/.test(request.url)) {
        // The stale IDE refresh token is revoked; agy's refresh token works.
        return /refresh_token=agy-refresh/.test(request.bodyText) ? json({access_token: 'agy-fresh', expires_in: 3600}) : {status: 400, bodyText: '{"error":"invalid_grant"}'};
      }
      if (auth !== 'Bearer agy-fresh') return {status: 401, bodyText: '{}'};
      if (/loadCodeAssist/.test(request.url)) return json({allowedTiers: []});
      // Consumer accounts have no project: the quota endpoint refuses with 403.
      if (/retrieveUserQuota/.test(request.url)) return {status: 403, bodyText: '{"error":{"code":403,"message":"no valid license"}}'};
      return json(models);
    }, host: {ls: {discoverStatus: () => ({status: 'missing'})}, keychain: {readGenericPassword: (service, account) => service === 'gemini' && account === 'antigravity' ? agy : null}}});
    h.ctx.app.pluginDataDir = h.home + '/agy plugin state';
    h.databases.set(h.normalize(h.ctx.host.fs.appSupportPath('Antigravity/User/globalStorage/state.vscdb')), oauthRow('stale-ide', 'stale-ide-refresh'));
    assert.equal(h.probe().lines[0].used, 25);
    assert(!h.calls.http.some(request => request.headers && request.headers.Authorization === 'Bearer agy-id-token'));
    assert(!h.calls.http.some(request => request.headers && request.headers.Authorization === 'Bearer agy-expired'));
    const refreshes = token => h.calls.http.filter(r => /oauth2\.googleapis\.com/.test(r.url) && r.bodyText.includes('refresh_token='+token)).length;
    assert.equal(h.probe().lines[0].used,25);
    assert.equal(refreshes('stale-ide-refresh'),1,'revoked DB refresh is not retried on every poll');
    assert.equal(refreshes('agy-refresh'),1,'agy uses its cached access token');
    const cooldown = [...h.files.keys()].find(path=>path.includes('oauth-retry-'));
    h.files.set(cooldown,JSON.stringify({retryAfterMs:Date.now()-1}));
    assert.equal(h.probe().lines[0].used,25);
    assert.equal(refreshes('stale-ide-refresh'),2,'cooldown expires');
    h.databases.set(h.normalize(h.ctx.host.fs.appSupportPath('Antigravity/User/globalStorage/state.vscdb')),oauthRow('stale-ide','replacement-refresh'));
    assert.equal(h.probe().lines[0].used,25);
    assert.equal(refreshes('replacement-refresh'),1,'new login bypasses old token cooldown');

  });

  test(`${platform}: Antigravity keeps independent profile caches and reads matching legacy caches`, () => {
    const h = providerHarness('antigravity', {platform, settings:{ideVariant:'antigravity'}, http:req=>{
      if (/oauth2\.googleapis\.com/.test(req.url)) return json({access_token:req.bodyText.includes('refresh_token=refresh-a')?'fresh-a':'fresh-b',expires_in:3600});
      return /^Bearer (fresh-[ab]|legacy)$/.test(req.headers.Authorization)?json(models):{status:401,bodyText:'{}'};
    }});
    h.ctx.app.pluginDataDir=h.home+'/independent caches';
    const db=h.normalize(h.ctx.host.fs.appSupportPath('Antigravity/User/globalStorage/state.vscdb'));
    for(const profile of ['a','b','a']){
      h.databases.set(db,oauthRow('stale-'+profile,'refresh-'+profile));
      assert.equal(h.probe().lines[0].used,25);
    }
    assert.equal(h.calls.http.filter(r=>/oauth2\.googleapis\.com/.test(r.url)).length,2);
    assert.equal([...h.files.keys()].filter(p=>/auth-[a-f0-9]+\.json$/.test(p)).length,2);
    h.databases.set(db,oauthRow('stale-c','refresh-c'));
    const originalPath=h.ctx.host.fs.appSupportPath('Antigravity/User/globalStorage/state.vscdb');
    const key=h.ctx.host.crypto.sha256Hex(originalPath+'\nrefresh-c');
    h.files.set(h.normalize(h.ctx.app.pluginDataDir+'/auth.json'),JSON.stringify({profileKey:key,accessToken:'legacy',expiresAtMs:Date.now()+3600000}));
    assert.equal(h.probe().lines[0].used,25);
    assert.equal(h.calls.http.filter(r=>/oauth2\.googleapis\.com/.test(r.url)).length,2,'matching legacy cache remains usable');
  });

  test(`${platform}: a consumer license 403 plus model outage does not refresh a valid token`, () => {
    const h=providerHarness('antigravity',{platform,http:req=>{
      if(/oauth2\.googleapis\.com/.test(req.url)) throw new Error('Unexpected refresh');
      if(/loadCodeAssist/.test(req.url)) return json({allowedTiers:[]});
      return /retrieveUserQuota/.test(req.url)?{status:403,bodyText:'{}'}:{status:503,bodyText:'{}'};
    },host:{ls:{discoverStatus:()=>({status:'missing'})},keychain:{readGenericPassword:()=>JSON.stringify({token:{access_token:'valid',refresh_token:'refresh',expiry:'2999-01-01T00:00:00Z'}})}}});
    h.ctx.app.pluginDataDir=h.home+'/outage cache';
    assert.throws(()=>h.probe());
    assert(!h.calls.http.some(r=>/oauth2\.googleapis\.com/.test(r.url)));
  });

  for(const id of ['antigravity','antigravity-cli']) test(`${platform}: ${id} prefers nested bearer over generic and ID tokens`, () => {
    const h=providerHarness(id,{platform,http:req=>{
      assert.equal(req.headers.Authorization,'Bearer bearer');
      return /loadCodeAssist/.test(req.url)?json({}):json(models);
    },host:{ls:{discoverStatus:()=>({status:'missing'})},keychain:{readGenericPassword:()=>JSON.stringify({token:'generic',id_token:'identity',credentials:{access_token:'bearer'}})}}});
    h.ctx.app.pluginDataDir=h.home+'/bearer cache';
    assert.equal(h.probe().lines[0].used,25);
  });

  test(`${platform}: Perplexity cache reader reports its actual platform limitation`, () => {
    const h = providerHarness('perplexity', {platform, settings: {cacheDbPath: 'selected-cache.db'}});
    if (platform === 'macos') {
      assert.throws(() => h.probe(), e => /Not logged in/.test(String(e)));
      assert.deepEqual(h.calls.files, [h.normalize('selected-cache.db')]);
    } else {
      assert.throws(() => h.probe(), e => e.code === 'unsupported' && /CFNetwork/.test(e.message));
      assert.equal(h.calls.files.length, 0);
    }
    assert.equal(h.calls.http.length, 0);
  });
}
