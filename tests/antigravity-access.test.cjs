const {test} = require('node:test');
const assert = require('node:assert/strict');
const {providerHarness} = require('./provider-harness.cjs');
const {response} = require('./provider-sync-harness.cjs');

const credentials = JSON.stringify({token:{access_token:'fixture-access',refresh_token:'fixture-refresh',expiry:'2999-01-01T00:00:00Z'}});
const models = {models:{gemini:{displayName:'Gemini Pro',quotaInfo:{remainingFraction:0.75}}}};
function harness(provider, http) {
  return providerHarness(provider, {host:{ls:{discoverStatus:()=>({status:'missing'})},keychain:{readGenericPassword:()=>credentials}},http});
}
for (const provider of ['antigravity','antigravity-cli']) {
  for (const [platform, architecture, suffix] of [['linux','x86_64','linux/amd64'],['windows','aarch64','windows/arm64'],['macos','aarch64','darwin/arm64']]) {
    test(`${provider}: Hub identity works on ${suffix} without rotating credentials`,()=>{
      const app=harness(provider,req=>{
        if(req.headers['User-Agent']!==`antigravity/hub/2.9.1 ${suffix}`) return response({},403);
        return req.url.endsWith(':fetchAvailableModels')?response(models):response({});
      });
      app.ctx.app.platform=platform;
      app.ctx.app.architecture=architecture;
      assert.equal(app.probe().lines.find(line=>line.type==='progress').used,25);
      assert.ok(app.calls.http.length>0);
      assert.ok(app.calls.http.every(req=>req.headers['User-Agent']===`antigravity/hub/2.9.1 ${suffix}`));
      assert.ok(!app.calls.http.some(req=>req.url.includes('oauth2.googleapis.com')));
    });
  }
  test(`${provider}: a quota permission denial is not an expired login or a measured zero`,()=>{
    const app=harness(provider,req=>req.url.endsWith(':loadCodeAssist')?response({allowedTiers:[]}):response({error:{status:'PERMISSION_DENIED'}},403));
    assert.throws(()=>app.probe(),error=>String(error).includes('Google denied Antigravity quota access for this account'));
    assert.ok(!app.calls.http.some(req=>req.url.includes('oauth2.googleapis.com')));
  });
  test(`${provider}: model quota fallback remains available after a summary permission denial`,()=>{
    const app=harness(provider,req=>{
      if(req.url.endsWith(':loadCodeAssist'))return response({});
      if(req.url.endsWith(':fetchAvailableModels'))return response(models);
      return response({},403);
    });
    const result=app.probe();
    assert.equal(result.lines.find(line=>line.type==='progress').used,25);
    assert.ok(!app.calls.http.some(req=>req.url.includes('oauth2.googleapis.com')));
  });
  test(`${provider}: an actual 401 can refresh once and then recover measured usage`,()=>{
    const app=harness(provider,req=>{
      if(req.url.includes('oauth2.googleapis.com'))return response({access_token:'fixture-renewed',expires_in:3600});
      if(req.headers.Authorization==='Bearer fixture-access')return response({},401);
      if(req.url.endsWith(':loadCodeAssist'))return response({});
      if(req.url.endsWith(':fetchAvailableModels'))return response(models);
      return response({},404);
    });
    const result=app.probe();
    assert.equal(result.lines.find(line=>line.type==='progress').used,25);
    assert.equal(app.calls.http.filter(req=>req.url.includes('oauth2.googleapis.com')).length,1);
  });
}
test('Antigravity retains a quota denial when the optional model endpoint is unavailable',()=>{
  const app=harness('antigravity',req=>{
    if(req.url.endsWith(':loadCodeAssist'))return response({});
    return response({},req.url.endsWith(':fetchAvailableModels')?503:403);
  });
  assert.throws(()=>app.probe(),error=>String(error).includes('Google denied Antigravity quota access'));
  assert.ok(!app.calls.http.some(req=>req.url.includes('oauth2.googleapis.com')));
});
