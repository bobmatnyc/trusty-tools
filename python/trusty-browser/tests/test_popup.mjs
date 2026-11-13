import assert from "node:assert/strict";
import {webcrypto} from "node:crypto";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const worker = await readFile(new URL("../src/trusty_browser/extension/worker.js",import.meta.url),"utf8");
const id="jabfhcolknbbdajilgpficniedhcfahc", origin="chrome-extension://"+id;
const email="user@example.org", url="http://127.0.0.1:1234/app", account="site-user";
function event() {
  let callback;
  return {addListener(fn){callback=fn;},fire(...args){return callback?.(...args);}};
}
async function fixture() {
  const stored = {}, requests = [];
  let refuse = false;
  const runtime = {id,onMessage:event(),onConnect:event(),onStartup:event(),onInstalled:event(),
    connectNative(name) {
      const port = {onMessage:event(),onDisconnect:event(),disconnect(){this.onDisconnect.fire();},
        postMessage(msg){
          if (name !== "com.trusty.browser.enroll") return;
          requests.push(msg);
          queueMicrotask(()=>this.onMessage.fire(refuse ? {ok:false,error:"NotTrusted"} : {ok:true,workflow:msg.workflow}));
        }};
      return port;
    }};
  const chrome = {runtime,tabs:{query:async()=>[{id:1,url}],sendMessage:async()=>({ok:true,url,account})},
    storage:{session:{get:async()=>({}),set:async()=>{}},local:{setAccessLevel:async()=>{},
      get:async keys=>Object.fromEntries([keys].flat().map(k=>[k,stored[k]])),
      set:async data=>Object.assign(stored,data),remove:async keys=>{for (const k of [keys].flat()) delete stored[k];}}},
    identity:{getProfileUserInfo:async()=>({email}),onSignInChanged:event()},
    alarms:{onAlarm:event(),create(){}}};
  vm.runInNewContext(worker,{chrome,crypto:webcrypto,TextEncoder,URL,importScripts(){},
    TrustyControlIndicator:{clearAll(){},clear(){}},clearTimeout(){},setTimeout(){return 0;}});
  runtime.onConnect.fire({name:"qa-document",sender:{id,url,frameId:0,documentId:"D",tab:{id:1}},onDisconnect:event()});
  const call=(msg)=>new Promise(resolve=>runtime.onMessage.fire(msg,{id,url:origin+"/setup.html"},resolve));
  return {call,stored,runtime,requests,refuse(v){refuse=v;}};
}
test("popup confirms the previewed route and resumes exactly the pending enrollment after refusal", async()=>{
  const f=await fixture();
  const {route}=await f.call({type:"preview"});
  assert.equal(route.account,account);
  assert.equal((await f.call({type:"enroll",workflow:"team",route:{...route,account:"different"}})).ok,false);
  assert.equal(f.requests.length,0);
  f.refuse(true);
  assert.equal((await f.call({type:"enroll",workflow:"team",route})).ok,false);
  const key=f.stored.pending.key;
  assert.match(key,/^[a-f0-9]{64}$/);
  assert.equal(f.stored.enrollment,undefined);
  f.refuse(false);
  assert.equal((await f.call({type:"enroll",workflow:"team",route})).ok,true);
  assert.equal(f.stored.enrollment.key,key);
  const response=await f.call({type:"preview"});
  assert.equal(response.enrolled,"team");
  assert.ok(!JSON.stringify(response).includes(key));
  assert.equal((await f.call({type:"enroll",workflow:"new",route})).ok,false);
  f.refuse(true);
  assert.equal((await f.call({type:"unenroll",workflow:"team"})).ok,false);
  assert.equal(f.stored.enrollment.key,key);
  f.refuse(false);
  assert.equal((await f.call({type:"unenroll",workflow:"team"})).ok,true);
  assert.equal(f.stored.enrollment,undefined);
});

test("a content script cannot initiate enrollment", async()=>{
  const f=await fixture();
  const accepted=f.runtime.onMessage.fire({type:"enroll",workflow:"bad"}, {id,url,frameId:0},()=>assert.fail());
  assert.equal(accepted,undefined);
  assert.equal(f.requests.length,0);
});
