const {test}=require('node:test');
const assert=require('node:assert/strict');
const {load,metric,response}=require('./provider-sync-harness.cjs');

const spaces={user:{notion_user:{user:{value:{id:'user',email:'fixture@example.test'}}},
  space:{workspace:{value:{value:{id:'workspace',name:'Work',subscription_tier:'business'}}}}}};
// Shape from upstream Tests/CodexBarTests/Fixtures/get-credit-rate-limit-status-migrated.json.
const migrated={status:'not_applicable',allowances:[
  {bucketId:'standard',cadence:'billing_period',state:'not_exposed'},
  {bucketId:'auto',cadence:'billing_period',state:'resolved',used:8.98,limit:100,periodStartMs:1793196000000,periodEndMs:1795874400000,enforcement:'enforced',credits:{used:134.66869,limit:1500}},
  {bucketId:'flex',cadence:'billing_period',state:'resolved',used:0,limit:100,periodStartMs:1793196000000,periodEndMs:1795874400000,enforcement:'enforced',credits:{used:0,limit:500}},
  {bucketId:'standard',cadence:'rolling',state:'not_exposed'},
]};

function notion(rateLimit,eligibility){
  return load('notion',{provider:{cookieHeader:'token'},request:req=>{
    if(req.url.endsWith('/getSpaces')) return response(spaces);
    if(req.url.endsWith('/getCreditRateLimitStatus')) return response(rateLimit);
    assert.ok(req.url.endsWith('/getAIUsageEligibilityV2'));
    assert.equal(req.timeoutMs,3000);
    return eligibility(req);
  }});
}

test('Notion reads personal allowance pools even when the legacy status is not_applicable',()=>{
  const result=notion(migrated,()=>response({premiumCredits:{totalCreditBalance:2500}})).probe();
  const standard=metric(result,'Standard'),select=metric(result,'Select');
  assert.equal(standard.used,134.66869);assert.equal(standard.limit,1500);
  assert.equal(standard.resetsAt,new Date(1795874400000).toISOString());
  assert.equal(standard.periodDurationMs,1795874400000-1793196000000);
  assert.equal(select.used,0);assert.equal(select.limit,500);
  assert.equal(metric(result,'Notion Credits').value,'2,500 remaining');
  assert.equal(metric(result,'Rolling'),undefined);
});

test('Notion credit balance failures never hide allowances',()=>{
  for(const eligibility of [()=>response({},500),()=>response('not json'),()=>response({premiumCredits:{totalCreditBalance:'12'}}),()=>{throw new Error('timeout');}]){
    const result=notion(migrated,eligibility).probe();
    assert.equal(metric(result,'Standard').used,134.66869);
    assert.equal(metric(result,'Notion Credits'),undefined);
  }
});

test('Notion skips ambiguous, unresolved and unmeasurable allowance entries',()=>{
  const entry={bucketId:'auto',cadence:'billing_period',state:'resolved',used:10,limit:100};
  for(const allowances of [[entry,entry],[{...entry,state:'pending'}],[{...entry,limit:0}],[{...entry,used:-1}],[{...entry,cadence:'rolling'}]]){
    const app=notion({status:'not_applicable',allowances},()=>response({}));
    assert.throws(app.probe,/did not provide usable personal AI allowances/);
  }
  const fallback=notion({status:'not_applicable',allowances:[{...entry,credits:{used:5}}]},()=>response({})).probe();
  assert.equal(metric(fallback,'Standard').used,10);assert.equal(metric(fallback,'Standard').format.kind,'percent');
});

test('Notion keeps the legacy schema and not_applicable workspaces unchanged',()=>{
  const result=notion({window:{used:5,limit:100,window:'5h'},resetsInSeconds:3600,
    billingPeriodWindow:{used:20,limit:100,periodEndMs:Date.parse('2026-09-30T00:00:00Z')}},()=>response({})).probe();
  assert.equal(metric(result,'Rolling').used,5);assert.equal(metric(result,'Monthly').used,20);
  assert.equal(metric(result,'Notion Credits'),undefined);
  const app=notion({status:'not_applicable'},()=>assert.fail('credits requested for an untracked workspace'));
  assert.throws(app.probe,/not tracked for Work/);
});

function amp(displayText){
  return load('amp',{files:{'~/.local/share/amp/secrets.json':JSON.stringify({'apiKey@https://ampcode.com/':'fixture'})},
    request:()=>response({ok:true,result:{displayText}})});
}

test('Amp enterprise-paid workspaces are usable without a fabricated balance',()=>{
  const result=amp('Signed in as member@example.test (member)\n**Workspace example:** usage paid by your enterprise - https://ampcode.com/workspaces/example').probe();
  assert.equal(result.plan,'Enterprise');
  assert.equal(metric(result,'Workspace example').value,'Enterprise');
  assert.equal(metric(result,'Credits'),undefined);assert.equal(metric(result,'Free'),undefined);
});

test('Amp Free percentage coexists with enterprise and balance workspaces',()=>{
  const result=amp('Signed in as member@example.test (member)\nAmp Free: 75% remaining today (resets daily)\nWorkspace example: usage paid by your enterprise\nWorkspace team: $12.50 remaining').probe();
  assert.equal(result.plan,'Free');
  const free=metric(result,'Free');assert.equal(free.used,25);assert.equal(free.limit,100);assert.equal(free.format.kind,'percent');
  assert.equal(metric(result,'Workspace example').value,'Enterprise');
  assert.equal(metric(result,'Workspace team').value,'$12.50');
  assert.equal(metric(result,'Credits'),undefined);
});

test('Amp dollar Free balance and individual credits are unchanged',()=>{
  const result=amp('Signed in as member@example.test\nAmp Free: $7.50/$10 remaining (replenishes +$0.42/hour)\nIndividual credits: $3.25 remaining').probe();
  assert.equal(result.plan,'Free');
  const free=metric(result,'Free');assert.equal(free.used,2.5);assert.equal(free.limit,10);assert.equal(free.format.kind,'dollars');
  assert.equal(metric(result,'Credits').value,'$3.25');
  assert.equal(amp('Individual credits: $3.25 remaining').probe().plan,'Credits');
  assert.throws(amp('Amp Free: unavailable').probe,/Could not parse usage data/);
});
