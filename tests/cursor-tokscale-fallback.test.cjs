const {test}=require('node:test');
const assert=require('node:assert/strict');
const {load,response}=require('./provider-sync-harness.cjs');

const token='e30.'+Buffer.from(JSON.stringify({sub:'fixture',exp:4102444800})).toString('base64url')+'.sig';
const usage=req=>response(req.url.endsWith('GetCurrentPeriodUsage')?{enabled:true,planUsage:{totalPercentUsed:10}}:{});
const day={date:'2026-09-04',totalTokens:135,inputTokens:115,outputTokens:20,costUsd:0.25};

function cursor(options,daily){
 const app=load(options.id||'cursor',{request:usage,...options});
 const calls=[];
 app.ctx.host.cursorPaths={resolveStateDb:()=>'/fixture.db',sharedCredentialsAllowed:options.id!=='cursor-nightly'};
 app.ctx.host.cursorUsageExport={queryDaily:opts=>{calls.push(opts);return daily;}};
 app.probe();
 return {calls,ingested:app.ingested.filter(row=>row.source!=='cursor_transcripts')};
}

test('Cursor requests the Tokscale fallback only for the default ambient account',()=>{
 const sqlite=()=>JSON.stringify([{value:token}]);
 const ok={status:'ok',data:{source:'dashboard',daily:[{...day,costKnown:true}]}};
 assert.equal(cursor({sqlite},ok).calls[0].localCache,true);
 assert.equal(cursor({credentials:{accessToken:token}},ok).calls[0].localCache,false);
 assert.equal(cursor({id:'cursor-nightly',sqlite},ok).calls[0].localCache,false);
 assert.equal(cursor({sqlite,provider:{instanceId:'other-cursor'}},ok).calls[0].localCache,false);
 // A blank configured credential falls through to sqlite auth but stays an explicit account.
 assert.equal(cursor({sqlite,credentials:{accessToken:' '}},ok).calls[0].localCache,false);
 assert.equal(cursor({credentials:{accessToken:'crossusage-dev-mock-access-token'}},ok).calls[0].localCache,false);
});

test('Cursor labels Tokscale cache rows separately from live billing and keeps unknown cost unknown',()=>{
 const sqlite=()=>JSON.stringify([{value:token}]);
 const live=cursor({sqlite},{status:'ok',data:{source:'dashboard',daily:[{...day,costKnown:true}]}}).ingested;
 assert.equal(live.length,1);assert.equal(live[0].source,'cursor_billing');assert.equal(live[0].daily[0].costUsd,0.25);

 const cached=cursor({sqlite},{status:'ok',data:{source:'tokscale',daily:[{...day,costKnown:true}]}}).ingested;
 assert.equal(cached.length,1);assert.equal(cached[0].source,'cursor_tokscale_cache');
 assert.equal(cached[0].costSource,'tokscale-local-cache');assert.equal(cached[0].daily[0].costUsd,0.25);

 const partial=cursor({sqlite},{status:'ok',data:{source:'tokscale',daily:[{...day,costUsd:0,costKnown:false}]}}).ingested;
 assert.equal(partial[0].daily[0].costKnown,false);assert.equal(partial[0].daily[0].costUsd,null);
 assert.equal(partial[0].daily[0].totalTokens,135);
});
