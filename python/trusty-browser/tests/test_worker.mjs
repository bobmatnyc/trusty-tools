import assert from "node:assert/strict";
import {createHmac, webcrypto} from "node:crypto";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../src/trusty_browser/extension/worker.js",import.meta.url),"utf8");
const id = "jabfhcolknbbdajilgpficniedhcfahc";
const profile = "example", email = "qa@example.org", secret = "a".repeat(64);
const route = "http://127.0.0.1:1234/app";

function event() {
  let cb;
  return {addListener(fn){cb = fn;}, fire(...args){return cb(...args);}};
}

async function fixture() {
  const sent = [], actions = [];
  let user = email;
  const port = {onMessage:event(),onDisconnect:event(), postMessage(m){sent.push(m);},
    disconnect(){this.closed=true;},closed:false};
  const local = {setAccessLevel:async()=>{},get:async()=>({enrollment:{profile,key:secret}})};
  const session = {get:async()=>({incarnation:"epoch-1"}),set:async()=>{}};
  const chrome = {storage:{local,session}, runtime:{id, onConnect:event(), onMessage:event(),
    onStartup:event(), onInstalled:event(),connectNative(){return port;}},
    identity:{getProfileUserInfo:async()=>({email:user}),onSignInChanged:event()},
    tabs:{get:async tab=>({id:tab}),sendMessage:async(id,msg,doc)=>{
      actions.push({id,msg,doc});
      return {ok:true,url:route,account:"site-user",result:{count:0}};
    }},alarms:{onAlarm:event(),create(){}}};
  vm.runInNewContext(source,{importScripts:()=>{},TrustyControlIndicator:{clear(){},clearAll(){}},chrome,crypto:webcrypto,TextEncoder,URL,setTimeout:()=>0,clearTimeout:()=>{}});
  for (let i=0;!sent.length && i<50;i++) await new Promise(r=>setImmediate(r));
  assert.equal(sent[0]?.type,"hello");
  return {chrome,port,sent,actions,switchAccount(){user="other@example.org";}};
}

function sign(role,h,serverNonce) {
  const data = JSON.stringify(["trusty-browser-native-v1",role,"chrome-extension://"+id,
    h.profile,h.email,h.incarnation,h.nonce,serverNonce]);
  return createHmac("sha256",secret).update(data).digest("hex");
}

test("extension does not reveal its key or sign a forged broker challenge", async () => {
  const {port,sent,actions} = await fixture();
  assert.ok(!JSON.stringify(sent).includes(secret));
  await port.onMessage.fire({type:"challenge",proof:"0".repeat(64),nonce:"1".repeat(64)});
  assert.equal(port.closed,true);
  assert.equal(sent.length,1);
  await port.onMessage.fire({id:1,op:"click"});
  assert.equal(actions.length,0);
});

test("valid role-bound proofs admit a pinned top-level document and recheck Chrome email", async () => {
  const f = await fixture();
  const h=f.sent[0], nonce="b".repeat(64);
  await f.port.onMessage.fire({type:"challenge",proof:sign("broker",h,nonce),nonce});
  assert.equal(f.sent[1].proof,sign("extension",h,nonce));
  await f.port.onMessage.fire({ok:true,generation:"g1"});
  f.chrome.runtime.onConnect.fire({sender:{id,url:route,frameId:0,tab:{id:7},documentId:"doc1"},
    name:"qa-document",onDisconnect:event()});
  const req = {id:"a",op:"inspect",url:route,email,account:"site-user"};
  await f.port.onMessage.fire(req);
  assert.equal(f.sent.at(-1).tab,7);
  assert.equal(f.actions[0].doc.documentId,"doc1");
  f.chrome.tabs.get = async id => {f.switchAccount();return {id};};
  await f.port.onMessage.fire({...req,id:"b",op:"click",tab:7,marker:"mark"});
  assert.equal(f.sent.at(-1).error,"ProfileMismatch");
  assert.equal(f.actions.length,1);
});
