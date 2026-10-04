(function(root,factory){
  const api=factory();
  if(typeof module==='object'&&module.exports)module.exports=api;
  else root.UsageDashboardBackend=api;
})(typeof globalThis==='object'?globalThis:this,function(){
  'use strict';
  const SESSION_KEY='usagestat.dashboard.collection-session';
  // Only the per-launch capability may survive a reload in this tab's
  // origin/port-scoped sessionStorage. The private setup key is never retained.
  function create(fetcher,sessionStore){
    if(sessionStore===undefined){try{sessionStore=globalThis.sessionStorage;}catch{}}
    let session=null,data=null;
    try{session=sessionStore?.getItem(SESSION_KEY)||null;}catch{}
    function remember(value){session=value;try{if(value)sessionStore?.setItem(SESSION_KEY,value);else sessionStore?.removeItem(SESSION_KEY);}catch{}}
    async function request(path,method='GET',body,setupKey){
      const headers={'X-Usagestat-Dashboard':'1'};
      if(session)headers['X-Usagestat-Session']=session;
      if(setupKey)headers['X-Usagestat-Setup-Key']=setupKey;
      if(body)headers['Content-Type']='application/json';
      const response=await fetcher(path,{method,headers,cache:'no-store',credentials:'same-origin',...(body&&{body:JSON.stringify(body)})});
      const result=await response.json();
      if(!response.ok){
        const error=new Error(response.status===409?'Collection settings changed in another window. Reload them before saving.':
          response.status===404?'Collection settings require a newer local backend with dashboard setup enabled.':
          response.status===403?'Open the dashboard directly on the local backend to edit collection settings.':
          response.status===400?'Some collection settings are invalid. Check the source, fields and values.':
          response.status===401?'Choose the backend’s private local setup key to edit collection settings.':'Could not access collection settings.');
        if(response.status===401){remember(null);data=null;}
        error.status=response.status;error.keyFile=result.keyFile;throw error;
      }
      return result;
    }
    return {
      get data(){return data;},
      async load(setupKey){
        const result=await request('/v1/settings/session','GET',null,setupKey);
        if(typeof result.token!=='string'||!result.token)throw new Error('Invalid dashboard session response.');
        remember(result.token);data=await request('/v1/settings');return data;
      },
      async save(patch){if(!data)throw new Error('Load collection settings first.');data=await request('/v1/settings','PATCH',{...patch,revision:data.revision});return data;},
      forget(){remember(null);data=null;},
    };
  }
  function modes(provider){const supported=provider?.supportedModes;return [...new Set(['auto',...(supported?.length?supported:['web','cli','oauth','api','local']),'custom'])];}
  function key(provider){return provider.instanceId||provider.id;}
  function settingRows(provider,settings={}){
    const rows=new Map();
    for(const field of provider?.setupFields||[]){
      if(!field||typeof field.key!=='string'||!['string','number','boolean'].includes(field.type)||
          !/^[a-zA-Z0-9][a-zA-Z0-9_.:-]{0,127}$/.test(field.key)||['__proto__','constructor','prototype'].includes(field.key))continue;
      rows.set(field.key,{type:field.type,title:field.title,hint:field.description,configured:false});
    }
    for(const [name,value] of Object.entries(settings))rows.set(name,{...rows.get(name),...value});
    return [...rows.entries()];
  }
  function settingValue(value,type){
    if(type==='boolean'){if(value==='true')return true;if(value==='false')return false;throw new Error('Use true or false for boolean settings.');}
    if(type==='number'){const number=Number(value);if(!String(value).trim()||!Number.isFinite(number))throw new Error('Enter a valid number.');return number;}
    return String(value);
  }
  return{create,modes,key,settingRows,settingValue,SESSION_KEY};
});
