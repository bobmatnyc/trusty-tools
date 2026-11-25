// A failed pairing handshake against the shipped worker (#11529): the extension in
// crates/trusty-browser/extension with mocked chrome APIs and a fake native port.
// Run: node --test crates/trusty-browser/tests/extension/
import assert from "node:assert/strict";
import {createHmac, webcrypto} from "node:crypto";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const dir = new URL("../../extension/", import.meta.url);
const names = ["worker.js", "pages.js", "pages_read.js", "codes.js", "emulation.js", "pages_frames.js"];
const [source, ...rest] = await Promise.all(names.map(f => readFile(new URL(f, dir), "utf8")));
const scripts = {"pages.js":rest[0], "pages_read.js":rest[1], "codes.js":rest[2], "emulation.js":rest[3],
  "pages_frames.js":rest[4],
  "indicator.js":""};
const id = "jabfhcolknbbdajilgpficniedhcfahc";
const email = "qa@example.org", secret = "a".repeat(64);
const PAIRED = {pairing:{profile:"5f0c6b1e-2d4a-4c8e-9b7a-1e2f3a4b5c6d", email, key:secret}};

function event() {
  let cb;
  return {addListener(fn){cb = fn;}, fire(...args){return cb(...args);}};
}
const settle = async () => { for (let i = 0; i < 30; i++) await new Promise(r => setImmediate(r)); };

// A worker that has sent its hello and waits for the broker; `stamp` is the asset stamp it runs.
async function fixture({stamp, local:localSeed = PAIRED} = {}) {
  const sent = [], timers = new Map(); let serial = 0, reloads = 0;
  const sessionStore = {incarnation:"epoch-1"}, localStore = structuredClone(localSeed);
  const native = {onMessage:event(), onDisconnect:event(), postMessage(m){sent.push(m);}, disconnect(){this.closed = true;}};
  const chrome = {
    runtime:{id, onConnect:event(), onMessage:event(), onStartup:event(), onInstalled:event(),
      getManifest:() => ({version:"0.2.0", version_name:"0.2.0+" + stamp}),
      reload(){ reloads++; },
      connectNative(){ return native; }},
    storage:{
      local:{setAccessLevel:async()=>{},
        get:async keys => Object.fromEntries([keys].flat().map(k => [k, structuredClone(localStore[k])])),
        set:async data => { Object.assign(localStore, structuredClone(data)); },
        remove:async keys => { for (const k of [keys].flat()) delete localStore[k]; }},
      session:{get:async key => ({[key]:structuredClone(sessionStore[key])}),
        set:async data => { Object.assign(sessionStore, structuredClone(data)); }}},
    identity:{getProfileUserInfo:async()=>({email}), onSignInChanged:event()},
    scripting:{getRegisteredContentScripts:async () => [], unregisterContentScripts:async () => {}},
    alarms:{onAlarm:event(), create(){}}};
  const ctx = vm.createContext({chrome, crypto:webcrypto, TextEncoder, URL,
    TrustyControlIndicator:{clear(){}, clearAll(){}, grants:async () => {}},
    setTimeout:(fn, ms) => { timers.set(++serial, {fn, ms}); return serial; }, clearTimeout:t => timers.delete(t)});
  ctx.importScripts = (...files) => { for (const f of files) vm.runInContext(scripts[f], ctx); };
  vm.runInContext(source, ctx);
  await settle();
  assert.equal(sent[0]?.type, "hello");
  const challenge = async () => {
    const hello = sent[0], nonce = "d".repeat(64);
    const data = JSON.stringify(["trusty-browser-native-v1", "broker", "chrome-extension://" + id,
      hello.profile, hello.email, hello.incarnation, hello.nonce, nonce]);
    await native.onMessage.fire({type:"challenge", proof:createHmac("sha256", secret).update(data).digest("hex"), nonce});
  };
  return {native, sent, timers, sessionStore, localStore, challenge, reloads:() => reloads};
}

test("a refusal that names a newer asset stamp reloads the extension once", async () => {
  const f = await fixture({stamp:"aaaaaaaaaaaaaaaa"});
  await f.challenge();
  await f.native.onMessage.fire({ok:false, error:"Unauthorized", assets:"bbbbbbbbbbbbbbbb"});
  await settle();
  assert.equal(f.reloads(), 1);
  const same = await fixture({stamp:"aaaaaaaaaaaaaaaa"});
  await same.challenge();
  await same.native.onMessage.fire({ok:false, error:"Unauthorized", assets:"aaaaaaaaaaaaaaaa"});
  await settle();
  assert.equal(same.reloads(), 0, "the running assets match");
  // The reloaded worker still runs the old assets: a second mismatched refusal must not reload again.
  const again = await fixture({stamp:"aaaaaaaaaaaaaaaa", local:f.localStore});
  await again.challenge();
  await again.native.onMessage.fire({ok:false, error:"Unauthorized", assets:"bbbbbbbbbbbbbbbb"});
  await settle();
  assert.equal(again.reloads(), 0);
});

test("a pairing handshake that fails is shown, never silent", async () => {
  const quiet = await fixture({stamp:"aaaaaaaaaaaaaaaa"});
  const [handshakeTimer] = [...quiet.timers.values()].filter(t => t.ms === 5000);
  await handshakeTimer.fn();
  await settle();
  assert.match(quiet.sessionStore.status, /^Pairing timed out/);
  const closed = await fixture({stamp:"aaaaaaaaaaaaaaaa"});
  closed.native.onDisconnect.fire();
  await settle();
  assert.match(closed.sessionStore.status, /^Pairing failed/);
});

test("a disconnect after the acknowledgement is an ordinary retry", async () => {
  const f = await fixture({stamp:"aaaaaaaaaaaaaaaa"});
  await f.challenge();
  await f.native.onMessage.fire({ok:true, generation:"g1", live:[], assets:"aaaaaaaaaaaaaaaa"});
  await settle();
  f.native.onDisconnect.fire();
  await settle();
  assert.equal(f.sessionStore.status, "Disconnected. Retrying…");
});
