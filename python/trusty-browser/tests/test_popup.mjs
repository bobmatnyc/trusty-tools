import assert from "node:assert/strict";
import {webcrypto} from "node:crypto";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const worker = await readFile(new URL("../src/trusty_browser/extension/worker.js",import.meta.url),"utf8");
const id="jabfhcolknbbdajilgpficniedhcfahc", origin="chrome-extension://"+id;
const email="user@example.org", url="http://127.0.0.1:1234/app", account="site-user";
const plain = value => JSON.parse(JSON.stringify(value));
function event() {
  let callback;
  return {addListener(fn){callback=fn;},fire(...args){return callback?.(...args);}};
}
async function fixture() {
  const stored = {}, requests = [];
  let refuse = null, during = null;
  const runtime = {id,onMessage:event(),onConnect:event(),onStartup:event(),onInstalled:event(),
    connectNative(name) {
      const port = {onMessage:event(),onDisconnect:event(),disconnect(){this.onDisconnect.fire();},
        postMessage(msg){
          if (name !== "com.trusty.browser.enroll") return;
          requests.push(msg);
          during?.(msg);
          queueMicrotask(()=>{
            if (refuse === "disconnect") return this.onDisconnect.fire();
            this.onMessage.fire(refuse || {ok:true,workflow:msg.op === "resolve" ? "browser-managed" : msg.workflow,state:"active"});
          });
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
  const open=(tab,documentId)=>runtime.onConnect.fire({name:"qa-document",
    sender:{id,url,frameId:0,documentId,tab:{id:tab}},onDisconnect:event()});
  open(1,"D");
  const call=(msg)=>new Promise(resolve=>runtime.onMessage.fire(msg,{id,url:origin+"/setup.html"},resolve));
  return {call,stored,runtime,requests,chrome,open,refuse(v){refuse=v;},during(fn){during=fn;}};
}
test("popup confirms the previewed route and resumes exactly the pending enrollment after refusal", async()=>{
  const f=await fixture();
  const {route}=await f.call({type:"preview"});
  assert.equal(route.account,account);
  assert.equal((await f.call({type:"enroll",workflow:"team",route:{...route,account:"different"}})).ok,false);
  assert.equal(f.requests.length,0);
  f.refuse("disconnect");
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
  f.refuse("disconnect");
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

test("invalid and definitively refused enrollment leaves no pending key", async()=>{
  const f=await fixture();
  const {route}=await f.call({type:"preview"});
  assert.equal((await f.call({type:"enroll",workflow:"Bad Name",route})).ok,false);
  assert.equal(f.requests.length,0);
  f.refuse({ok:false,error:"InvalidEnrollment"});
  assert.equal((await f.call({type:"enroll",workflow:"team",route})).ok,false);
  assert.equal(f.stored.pending,undefined);
  f.refuse(null);
  assert.equal((await f.call({type:"enroll",workflow:"other",route})).ok,true);
});
test("workflow transfer requires explicit matching confirmation and can be cancelled", async()=>{
  const f=await fixture();
  const {route}=await f.call({type:"preview"});
  const existing={email:"old@example.org",account:"old",url:"https://old.example.org/qa/app",
    version:"a".repeat(64)};
  f.refuse({ok:false,error:"EnrollmentConflict",previous:existing});
  const present=()=>f.call({type:"enroll",workflow:"team",route});
  const preview=await present();
  assert.equal(preview.previous.account,"old");
  assert.equal(f.stored.pending,undefined);
  await f.call({type:"cancel",confirmation:preview.confirmation});
  const replace=confirmation=>f.call({type:"replace",workflow:"team",route,confirmation});
  assert.equal((await replace(preview.confirmation)).ok,false);
  assert.equal(f.requests.length,1);
  const next=await present();
  f.refuse({ok:false,error:"ReplacementStale"});
  assert.equal((await replace(next.confirmation)).error,"ReplacementStale");
  assert.equal(f.stored.pending,undefined);
  f.refuse({ok:false,error:"EnrollmentConflict",previous:{...existing,version:"b".repeat(64)}});
  const latest=await present();
  f.refuse(null);
  assert.equal((await replace(latest.confirmation)).ok,true);
  assert.equal(f.requests.at(-1).previous_version,"b".repeat(64));
});
test("clear enrollment only on verified bootstrap tombstone",async()=>{
  const f=await fixture(),{route}=await f.call({type:"preview"});
  await f.call({type:"enroll",workflow:"team",route});
  f.refuse({ok:false,error:"Unauthorized"});
  assert.equal((await f.call({type:"preview"})).ok,false);
  assert.ok(f.stored.enrollment);
  f.refuse({ok:true,workflow:"team",state:"revoked"});
  assert.equal((await f.call({type:"preview"})).enrolled,undefined);
  assert.equal(f.stored.enrollment,undefined);
});

test("only a pre-commit occupied name clears the pending enrollment", async()=>{
  const f=await fixture();
  const {route}=await f.call({type:"preview"});
  const enroll=workflow=>f.call({type:"enroll",workflow,route});
  f.refuse({ok:false,error:"WorkflowCredentialOccupied"});
  assert.equal((await enroll("occupied")).error,"WorkflowCredentialOccupied");
  assert.equal(f.stored.pending,undefined);
  f.refuse({ok:false,error:"CredentialConflict"});
  assert.equal((await enroll("other")).error,"CredentialConflict");
  const key=f.stored.pending.key;
  assert.equal((await enroll("wrong")).ok,false);
  f.refuse(null);
  assert.equal((await enroll("other")).ok,true);
  assert.equal(f.stored.enrollment.key,key);
});

test("one-click activation uses the service-selected workflow and resumes pending grants", async()=>{
  const f = await fixture();
  const {route} = await f.call({type:"preview"});
  const response = await f.call({type:"activate",route,workflow:"ignored"});
  assert.equal(response.workflow,"browser-managed");
  assert.deepEqual(f.requests.map(m => m.op),["resolve","enroll"]);
  assert.equal(f.requests[0].email,email);
  assert.equal(f.requests[0].account,account);
  assert.equal(f.requests[0].url,url);
  assert.equal(f.stored.enrollment.workflow,"browser-managed");

  const retry = await fixture();
  const preview = await retry.call({type:"preview"});
  retry.refuse("disconnect");
  await retry.call({type:"enroll",workflow:"prior-name",route:preview.route});
  const key = retry.stored.pending.key;
  retry.refuse(null);
  const resumed = await retry.call({type:"activate",route:preview.route});
  assert.equal(resumed.workflow,"prior-name");
  assert.equal(retry.stored.enrollment.key,key);
  assert.deepEqual(retry.requests.map(m => m.op),["enroll","enroll"]);
});

test("activation refuses a malformed resolve reply and stores nothing", async()=>{
  const f = await fixture();
  const {route} = await f.call({type:"preview"});
  for (const reply of [{ok:true,workflow:"Bad Name"},{ok:true},{ok:true,workflow:"-lead"},{ok:true,workflow:7}]) {
    f.refuse(reply);
    const response = await f.call({type:"activate",route});
    assert.deepEqual(plain(response),{ok:false,error:"Invalid enrollment reply"});
    assert.equal(f.stored.pending,undefined);
    assert.equal(f.stored.enrollment,undefined);
  }
  assert.deepEqual(f.requests.map(m => m.op),["resolve","resolve","resolve","resolve"]);
  f.refuse(null);
  assert.equal((await f.call({type:"activate",route})).workflow,"browser-managed");
});

test("activation refuses when the tab or document changes during resolve", async()=>{
  for (const change of ["tab","document"]) {
    const f = await fixture();
    const {route} = await f.call({type:"preview"});
    const query = f.chrome.tabs.query;
    f.during(msg => {
      if (msg.op !== "resolve") return;
      if (change === "tab") {
        f.open(2,"D2");
        f.chrome.tabs.query = async()=>[{id:2,url}];
      } else {
        f.open(1,"E");
      }
    });
    const response = await f.call({type:"activate",route});
    assert.deepEqual(plain(response),{ok:false,error:"The profile, tab or site changed. Reopen the popup."},change);
    assert.deepEqual(f.requests.map(m => m.op),["resolve"],change);
    assert.equal(f.stored.pending,undefined,change);
    assert.equal(f.stored.enrollment,undefined,change);
    f.during(null);
    f.chrome.tabs.query = query;
    if (change === "document") f.open(1,"D");
    assert.equal((await f.call({type:"activate",route})).workflow,"browser-managed",change);
  }
});

test("activation refuses when the pending or stored enrollment changes during resolve", async()=>{
  const injected = {pending:{key:"f".repeat(64),workflow:"elsewhere",email,url,account,profile:"p"},
    enrollment:{key:"e".repeat(64),workflow:"elsewhere",email,url,account,profile:"p"}};
  for (const field of ["pending","enrollment"]) {
    const f = await fixture();
    const {route} = await f.call({type:"preview"});
    f.during(msg => { if (msg.op === "resolve") f.stored[field] = injected[field]; });
    const response = await f.call({type:"activate",route});
    assert.deepEqual(plain(response),{ok:false,error:"Enrollment changed. Reopen the popup."},field);
    assert.deepEqual(f.requests.map(m => m.op),["resolve"],field);
    assert.deepEqual(f.stored,{[field]:injected[field]},field);
    f.during(null);
    delete f.stored[field];
    assert.equal((await f.call({type:"activate",route})).workflow,"browser-managed",field);
  }
});

test("enabling a site refuses a tab id or URL that is not the active tab", async()=>{
  const f = await fixture();
  const page = "https://site.test/app", pattern = "https://site.test/*", calls = [];
  f.chrome.tabs.query = async()=>[{id:1,url:page}];
  f.chrome.permissions = {contains:async q=>{calls.push(["contains",q]);return true;}};
  f.chrome.scripting = {getRegisteredContentScripts:async()=>[],
    registerContentScripts:async s=>{calls.push(["register",s]);},
    executeScript:async s=>{calls.push(["execute",s]);}};
  for (const wrong of [{tab:2,url:page},{tab:1,url:"https://site.test/other"},{tab:"1",url:page}]) {
    const response = await f.call({type:"enable",pattern,...wrong});
    assert.deepEqual(plain(response),{ok:false,error:"Access to this site has not been granted."},JSON.stringify(wrong));
    assert.deepEqual(calls,[],JSON.stringify(wrong));
  }
  assert.deepEqual(plain(await f.call({type:"enable",pattern,tab:1,url:page})),{ok:true});
  assert.deepEqual(calls.map(c => c[0]),["contains","register"]);
});
