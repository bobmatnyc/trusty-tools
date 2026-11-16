import assert from "node:assert/strict";
import {createHmac, webcrypto} from "node:crypto";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../src/trusty_browser/extension/worker.js",import.meta.url),"utf8");
const pagesSource = await readFile(new URL("../src/trusty_browser/extension/pages.js",import.meta.url),"utf8");
const codesSource = await readFile(new URL("../src/trusty_browser/extension/codes.js",import.meta.url),"utf8");
const readSource = await readFile(new URL("../src/trusty_browser/extension/pages_read.js",import.meta.url),"utf8");
// importScripts by file name: pages.js loads pages_read.js itself (#11355); the indicator is stubbed.
const scripts = {"pages.js":pagesSource, "pages_read.js":readSource, "codes.js":codesSource, "indicator.js":""};
const loader = ctx => (...files) => { for (const file of files) vm.runInContext(scripts[file], ctx); };
const id = "jabfhcolknbbdajilgpficniedhcfahc";
// #11356: every page_read frame carries its read controls.
const READ = {scope_ref:null, max_chars:50000, viewport_only:false, include_text:false};
const profile = "example", email = "qa@example.org", secret = "a".repeat(64);
const route = "http://127.0.0.1:1234/app";

function event() {
  let cb;
  return {addListener(fn){cb = fn;}, fire(...args){return cb(...args);}};
}

async function fixture() {
  const sent = [], actions = [];
  let user = email;
  const timers = new Map();let serial=0;
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
  const ctx = vm.createContext({TrustyControlIndicator:{clear(){},clearAll(){}},chrome,crypto:webcrypto,TextEncoder,URL,setTimeout:(fn,ms)=>{timers.set(++serial,{fn,ms});return serial;},clearTimeout:id=>timers.delete(id)});
  ctx.importScripts = loader(ctx);
  vm.runInContext(source,ctx);
  for (let i=0;!sent.length && i<50;i++) await new Promise(r=>setImmediate(r));
  assert.equal(sent[0]?.type,"hello");
  return {chrome,port,sent,actions,timers,switchAccount(){user="other@example.org";}};
}

function sign(role,h,serverNonce,key=secret) {
  const data = JSON.stringify(["trusty-browser-native-v1",role,"chrome-extension://"+id,
    h.profile,h.email,h.incarnation,h.nonce,serverNonce]);
  return createHmac("sha256",key).update(data).digest("hex");
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

test("valid role-bound proofs admit the broker and the removed workflow ops answer InvalidOperation", async () => {
  const f = await fixture();
  const h=f.sent[0], nonce="b".repeat(64);
  await f.port.onMessage.fire({type:"challenge",proof:sign("broker",h,nonce),nonce});
  assert.equal(f.sent[1].proof,sign("extension",h,nonce));
  await f.port.onMessage.fire({ok:true,generation:"g1",live:[]});
  // #11239: the site adapter (content.js) and its workflow ops are gone.
  for (const op of ["inspect","snapshot","click","lease_show","lease_clear"]) {
    await f.port.onMessage.fire({id:op,op,url:route,email,account:"site-user",tab:7});
    assert.deepEqual(plain(f.sent.at(-1)),{id:op,ok:false,error:"InvalidOperation"});
  }
  assert.equal(f.actions.length,0);
});

test("duplicate authenticated registration backs off then reconnects with fresh proof",async()=>{
  const f=await fixture(), nonce="c".repeat(64);
  const challenge=hello=>f.port.onMessage.fire({type:"challenge",nonce,proof:sign("broker",hello,nonce)});
  await challenge(f.sent[0]);
  await f.port.onMessage.fire({ok:false,error:"DuplicateProfile"});
  assert.equal(f.port.closed,true);
  const retry=[...f.timers.values()].find(job=>job.ms===250);
  assert.ok(retry);
  await retry.fn();
  const hello=f.sent.at(-1);
  assert.equal(hello.type,"hello");
  assert.notEqual(hello.nonce,f.sent[0].nonce);
  await challenge(hello);
  await f.port.onMessage.fire({ok:true,generation:"new-connection",live:[]});
  assert.equal(f.sent.at(-1).proof,sign("extension",hello,nonce));
});

// ---- #11239: activation-code grants, pairing and page commands -----------------------------------
const settle = async () => { for (let i = 0; i < 30; i++) await new Promise(r => setImmediate(r)); };
const plain = value => JSON.parse(JSON.stringify(value));
const ENROLLED = {enrollment:{profile, key:secret, email, workflow:"w", url:route, account:"a"}};
const PAIRED = {pairing:{profile:"5f0c6b1e-2d4a-4c8e-9b7a-1e2f3a4b5c6d", email, key:"b".repeat(64)}};

// A browser with explicit tabs, a fake broker behind both native hosts, and the real worker table.
async function codeFixture({session:seed = {}, held = false, tabs:layout, live = [], local:localSeed = ENROLLED,
  ack, stamp:manifestStamp = null, incarnation:life = "epoch-1"} = {}) {
  const sent = [], ran = [], queries = [], updates = [], brokerCalls = [], alarms = [], badge = [], cdp = [], switches = [], focused = [];
  const timers = new Map(); let serial = 0, mintSerial = 0, generation = 0, reloads = 0;
  const tabs = new Map(Object.entries(layout || {
    7:{origin:"https://a.test", title:"Alpha", active:true},
    8:{origin:"https://b.test", title:"Beta", active:false},
    9:{origin:"https://c.test", title:"Gamma", active:false},
  }).map(([id, t]) => [Number(id), {doc:"doc-" + id, ...t}]));
  const perms = {held, removed:0, requested:0};
  const sessionStore = {...(life === null ? {} : {incarnation:life}), ...structuredClone(seed)};
  const localStore = structuredClone(localSeed);
  const failures = {sessionSet:false};
  const listeners = new Set();
  const ok = {grants:[...live]};
  const fakeBroker = msg => {
    if (msg.op === "connect") {
      const live = ok.grants.find(g => g.state === "active" && g.scope === "profile");
      if (live) return {ok:true, grant:live.grant, scope:"profile", profile:msg.profile};
      const grant = "g-" + String(++mintSerial).padStart(12, "0");
      ok.grants.push({grant, scope:"profile", state:"active", agents:0, idle_s:0});
      return {ok:true, grant, scope:"profile", profile:msg.profile};
    }
    if (msg.op === "grants") return {ok:true, grants:ok.grants};
    if (msg.op === "revoke_grant") {
      const before = ok.grants.length;
      ok.grants = ok.grants.filter(g => g.grant !== msg.grant);
      return before === ok.grants.length ? {ok:false, error:"UnknownGrant"} : {ok:true, revoked:1};
    }
    if (msg.op === "revoke_grants") { const revoked = ok.grants.length; ok.grants = []; return {ok:true, revoked}; }
    if (msg.op === "pair" || msg.op === "unpair") return {ok:true, profile:msg.profile};
    return {ok:false, error:"InvalidRequest"};
  };
  let broker = fakeBroker;
  let moves = 0;
  const go = (tab, t, next) => {
    const doc = "doc-" + tab + "-" + ++moves;
    // #11354: a slow navigation keeps the old document current until `land` commits it.
    const commit = () => { if (next !== t.origin) t.activeTab = false; t.origin = next; t.doc = doc; t.status = "complete"; };
    if (t.slow) {
      t.status = "loading";
      setImmediate(() => { for (const l of [...listeners]) l(tab, {status:"loading"}); });
      t.land = () => { commit(); for (const l of [...listeners]) l(tab, {status:"complete"}); };
    } else {
      commit();
      setImmediate(() => { for (const l of [...listeners]) l(tab, {status:"loading"}); for (const l of [...listeners]) l(tab, {status:"complete"}); });
    }
    return {id:tab};
  };
  const accessible = t => t && (t.activeTab || (perms.held && t.origin.startsWith("https:")));
  const native = {onMessage:event(), onDisconnect:event(), postMessage(m){sent.push(m);}, disconnect(){this.closed = true;}};
  const chrome = {
    runtime:{id, onConnect:event(), onMessage:event(), onStartup:event(), onInstalled:event(),
      getManifest:() => ({version:"0.2.0", version_name:"0.2.0+" + (manifestStamp ?? "")}),
      // Chrome clears storage.session on chrome.runtime.reload().
      reload(){ reloads++; for (const k of Object.keys(sessionStore)) delete sessionStore[k]; },
      connectNative(name) {
        if (name === "com.trusty.browser") { native.closed = false; return native; }
        const port = {onMessage:event(), onDisconnect:event(), disconnect(){},
          postMessage(msg) {
            brokerCalls.push(structuredClone(msg));
            queueMicrotask(() => {
              const reply = broker(msg);
              if (reply === "disconnect") port.onDisconnect.fire(); else port.onMessage.fire(reply);
            });
          }};
        return port;
      }},
    storage:{
      local:{setAccessLevel:async()=>{},
        get:async keys => Object.fromEntries([keys].flat().map(k => [k, structuredClone(localStore[k])])),
        set:async data => { Object.assign(localStore, structuredClone(data)); },
        remove:async keys => { for (const k of [keys].flat()) delete localStore[k]; }},
      session:{
        get:async key => ({[key]:structuredClone(sessionStore[key])}),
        set:async data => { if (failures.sessionSet) throw new Error("quota"); Object.assign(sessionStore, structuredClone(data)); },
      }},
    identity:{getProfileUserInfo:async()=>({email}), onSignInChanged:event()},
    permissions:{
      request:async()=>{ perms.requested++; perms.held = true; return true; },
      contains:async()=>perms.held,
      remove:async()=>{ perms.removed++; const was = perms.held; perms.held = false; return was; },
    },
    alarms:{onAlarm:event(), create(name, info){ alarms.push({name, ...info}); }},
    windows:{update:async (win, props) => { focused.push({win, ...props}); return {id:win}; }},
    tabs:{
      get:async tab => { if (!tabs.has(tab)) throw new Error("No tab with id: " + tab); return {id:tab, status:tabs.get(tab).status || "complete"}; },
      query:async q => { queries.push(q); return [...tabs].map(([tid, t], index) => ({id:tid, windowId:1, index, active:t.active})); },
      update:async (tab, {url, active}) => {
        const t = tabs.get(tab);
        if (!t) throw new Error("No tab");
        if (active === true) {
          for (const other of tabs.values()) other.active = false;
          t.active = true;
          switches.push(tab);
          return {id:tab, windowId:1};
        }
        updates.push({tab, url});
        (t.past ||= []).push(t.origin); t.ahead = [];
        return go(tab, t, new URL(url).origin);
      },
      // #11292: history of origins per tab; Chrome rejects goBack/goForward with nothing to go to.
      goBack:async tab => { const t = tabs.get(tab); if (!t?.past?.length) throw new Error("Cannot find a next page in history.");
        (t.ahead ||= []).push(t.origin); return go(tab, t, t.past.pop()); },
      goForward:async tab => { const t = tabs.get(tab); if (!t?.ahead?.length) throw new Error("Cannot find a next page in history.");
        t.past.push(t.origin); return go(tab, t, t.ahead.pop()); },
      reload:async (tab, props) => { const t = tabs.get(tab); if (!t) throw new Error("No tab"); t.reloads = [...(t.reloads || []), props];
        return go(tab, t, t.origin); },
      onUpdated:{addListener(l){listeners.add(l);}, removeListener(l){listeners.delete(l);}},
    },
    scripting:{getRegisteredContentScripts:async () => [], unregisterContentScripts:async () => {},
      executeScript:async ({target, func, args}) => {
      const t = tabs.get(target.tabId);
      if (!accessible(t)) throw new Error("Cannot access contents of the page");
      if (target.documentIds && !target.documentIds.includes(t.doc)) throw new Error("No document with id");
      if (func.name !== "pageProbe") ran.push({tab:target.tabId, func:func.name, args});
      const result = func.name === "pageProbe" ? [t.origin, t.title]
        : func.name === "pageInspect" ? t.readResult ?? {origin:t.origin, url:t.origin + "/secret-path?q=1", title:t.title,
          text:"private page text", elements:[{ref:"1.0", tag:"button", role:"button", name:"Go"}], truncated:false}
        : t.result || (func.name === "pageDrag" ? {ok:true, dropped:true} : {ok:true, x:10, y:20});
      if (t.throwAfterRun && func.name !== "pageProbe") throw new Error("script error");
      return [{frameId:0, documentId:t.doc, result}];
    }},
    // #11294: CDP input. `t.dialog` opens one native dialog at the mouse press (`t.dialogFails` makes its
    // answer fail); `t.busy` refuses attach; `t.moveAt` names an event type after which the tab gets
    // `t.move` ({doc} or {origin}); `t.hang` names a method that answers only
    // when the test calls `t.release`. Like Chrome, attach needs
    // no host access.
    debugger:{onEvent:{fns:new Set(), addListener(fn){this.fns.add(fn);}, removeListener(fn){this.fns.delete(fn);},
        fire(...args){ for (const fn of [...this.fns]) fn(...args); }},
      attach:async ({tabId}) => {
        const t = tabs.get(tabId);
        if (t?.busy) throw new Error("Another debugger is already attached");
        cdp.push({tab:tabId, method:"attach"});
      },
      detach:async ({tabId}) => { cdp.push({tab:tabId, method:"detach"}); },
      sendCommand:async ({tabId}, method, params) => {
        cdp.push({tab:tabId, method, params});
        const t = tabs.get(tabId);
        if (t.hang === method) return new Promise(done => { t.release = () => done({}); });
        if (t.dialogFails && method === "Page.handleJavaScriptDialog") throw new Error("No dialog is showing");
        if (t.dialog && method === "Input.dispatchMouseEvent" && params.type === "mousePressed")
          await chrome.debugger.onEvent.fire({tabId}, "Page.javascriptDialogOpening", t.dialog);
        if (t.moveAt && params?.type === t.moveAt) Object.assign(t, t.move);
        return {};
      }},
  };
  const ctx = vm.createContext({chrome, crypto:webcrypto, TextEncoder, URL,
    TrustyControlIndicator:{clear(){}, clearAll(){}, grants:async n => { badge.push(n); }},
    setTimeout:(fn, ms) => { timers.set(++serial, {fn, ms}); return serial; }, clearTimeout:t => timers.delete(t)});
  ctx.importScripts = loader(ctx);
  vm.runInContext(source, ctx);
  await settle();
  // The broker's pairing acknowledgement; `liveIds` defaults to everything the fake broker lists.
  const handshake = async (liveIds, extra = {}) => {
    const hello = sent.findLast(m => m.type === "hello"), nonce = "d".repeat(64);
    const key = (localStore.enrollment || localStore.pairing).key;
    await native.onMessage.fire({type:"challenge", proof:sign("broker", hello, nonce, key), nonce});
    await native.onMessage.fire({ok:true, generation:"g" + ++generation, live:liveIds ?? ok.grants.map(g => g.grant), ...extra});
    await settle();
  };
  if (ack !== false && sent.length) await handshake(ack);
  let seq = 0;
  const command = async (op, fields = {}) => {
    const id = "c" + ++seq;
    // The broker always sends a dialog policy with a click (#11294).
    if (op === "page_click" && !("dialog" in fields)) fields = {...fields, dialog:"dismiss"};
    await native.onMessage.fire({id, op, email, ...fields});
    await settle();
    return plain(sent.findLast(m => m.id === id));
  };
  // #11239: a listener that does not claim the message never replies; Chrome then rejects
  // sendMessage, so the fixture rejects too instead of leaving the test pending forever.
  const popup = msg => new Promise((resolve, reject) => {
    if (chrome.runtime.onMessage.fire(msg, {id, url:"chrome-extension://" + id + "/setup.html"}, resolve) !== true) {
      reject(new Error("The message port closed before a response was received."));
    }
  }).then(async reply => {
    await settle();
    return plain(reply);
  });
  const table = () => plain(sessionStore.grants || {});
  return {chrome, native, sent, ran, cdp, queries, updates, switches, focused, brokerCalls, alarms, badge, timers, tabs, perms, failures,
    sessionStore, localStore, command, popup, table, handshake, live:ok, fakeBroker, reloads:() => reloads,
    setBroker(fn){ broker = fn; },
    end:(grant, last, reason = "closed") => command("grant_end", {grant, reason, last}),
    // The broker's own end: it drops the grant from its list, then sends grant_end with `last`.
    brokerEnds(grant, reason = "revoked") {
      ok.grants = ok.grants.filter(g => g.grant !== grant);
      return command("grant_end", {grant, reason, last:ok.grants.length === 0});
    }};
}
// The popup requests https://*/* inside the Connect click, then sends `connect`.
const connectGrant = async f => {
  await f.chrome.permissions.request({origins:["https://*/*"]});
  return f.popup({type:"connect"});
};
// A grant row the broker also lists, for table and scope cases that need more than one row.
const seedRow = (f, grant, scope = "profile", tab = null) => {
  f.sessionStore.grants = {...(f.sessionStore.grants || {}), [grant]:{scope, tab}};
  f.live.grants.push({grant, scope, ...(tab === null ? {} : {tab}), state:"active", idle_s:0});
  return grant;
};

test("a tab row never reaches another tab", async () => {
  const f = await codeFixture({held:true, tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true},
    8:{origin:"https://b.test", title:"B", active:false}}});
  const grant = seedRow(f, "g-0000000000a7", "tab", 7);
  const before = {ran:f.ran.length, updates:f.updates.length};
  for (const [op, extra] of [["page_tab",{}], ["page_read",{document:null, ...READ}], ["page_click",{document:"doc-8", ref:"1.0"}],
    ["page_type",{document:"doc-8", ref:"1.0", text:"x"}], ["page_navigate",{url:"https://b.test/next"}],
    ["page_press",{document:"doc-8", ref:null, key:"Enter", modifiers:[]}]]) {
    const reply = await f.command(op, {grant, tab:8, ...extra});
    assert.deepEqual(reply, {id:reply.id, ok:false, error:"OutOfScope", tab:8}, op);
  }
  assert.deepEqual({ran:f.ran.length, updates:f.updates.length}, before);
  // A probe names the granted tab and the grant explicitly.
  const probe = await f.command("page_tab", {grant, tab:7});
  assert.deepEqual(probe, {id:probe.id, ok:true, email, tab:7,
    result:{access:true, document:"doc-7", origin:"https://a.test", title:"A"}});
  const listed = await f.command("page_tabs", {grant});
  assert.deepEqual(listed.result.tabs.map(t => t.tab), [7]);
  assert.equal((await f.command("page_read", {grant, tab:7, document:null, ...READ})).ok, true);
  assert.ok(f.queries.every(q => !("active" in q)), JSON.stringify(f.queries));
});

test("stale document refuses before the page runs", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  for (const [op, extra] of [["page_click",{ref:"1.0"}], ["page_hover",{ref:"1.0"}],
    ["page_type",{ref:"1.0", text:"secret typed"}],
    ["page_press",{ref:null, key:"Tab", modifiers:["Shift"]}]]) {
    for (const document of ["doc-old", null]) {
      const reply = await f.command(op, {grant, tab:7, document, ...extra});
      assert.deepEqual(reply, {id:reply.id, ok:false, error:"DocumentChanged", tab:7, access:true, document:"doc-7"}, op);
    }
  }
  assert.deepEqual(f.ran, []);
  const done = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(done.result, {document:"doc-7"});
  assert.equal(f.ran.length, 1);
  // #11287: hover runs the injected pageHover inside the lease document only.
  const hovered = await f.command("page_hover", {grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(hovered.result, {document:"doc-7"});
  assert.equal(f.ran.at(-1).func, "pageHover");
});

test("scroll runs in the lease's document and relays its view", async () => {
  // #11291
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const stale = await f.command("page_scroll", {grant, tab:7, document:"doc-old", to:"bottom", ref:null, wait_ms:500});
  assert.equal(stale.error, "DocumentChanged");
  assert.deepEqual(f.ran, []);
  const view = {scroll_y:540, scroll_height:4000, viewport_height:600, grew:true, at_end:false};
  f.tabs.get(7).result = {ok:true, view};
  const done = await f.command("page_scroll", {grant, tab:7, document:"doc-7", to:"bottom", ref:null, wait_ms:500});
  assert.deepEqual(done.result, {document:"doc-7", ...view});
  assert.equal(JSON.stringify(f.ran.map(r => [r.func, r.args])), JSON.stringify([["pageScroll", ["bottom", null, 500]]]));
  f.tabs.get(7).result = {error:"StaleElement"};
  const gone = await f.command("page_scroll", {grant, tab:7, document:"doc-7", to:"element", ref:"1.0", wait_ms:0});
  assert.deepEqual(gone, {id:gone.id, ok:false, error:"StaleElement", tab:7});
});

// #11356: the read controls reach pageInspect unchanged; a bad one refuses before the page runs.
test("page_read passes its controls to the page and relays a stale scope", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const controls = {scope_ref:"5.2", max_chars:2000, viewport_only:true, include_text:true};
  assert.equal((await f.command("page_read", {grant, tab:7, document:null, ...controls})).ok, true);
  assert.equal(JSON.stringify(f.ran.at(-1)), JSON.stringify({tab:7, func:"pageInspect", args:["read", 512, controls]}));
  const ran = f.ran.length;
  for (const bad of [{max_chars:999}, {max_chars:250001}, {scope_ref:"x"}, {viewport_only:1}, {include_text:null}]) {
    const reply = await f.command("page_read", {grant, tab:7, document:null, ...READ, ...bad});
    assert.equal(reply.error, "InvalidRequest", JSON.stringify(bad));
  }
  assert.equal(f.ran.length, ran);
  f.tabs.get(7).readResult = {error:"StaleElement"};
  const stale = await f.command("page_read", {grant, tab:7, document:null, ...controls});
  assert.deepEqual(stale, {id:stale.id, ok:false, error:"StaleElement", tab:7});
});

// #11357: page_find injects pageInspect in find mode with its terms; it needs a term and relays a stale scope.
test("page_find passes its terms to the page and relays its matches", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const terms = {role:"button", name:"Delete", text:null, within_ref:"3.1", limit:5};
  const match = {ref:"3.2", tag:"button", role:"button", name:"Delete", in:{role:"row", name:"Ann Delete", ref:"3.1"}};
  f.tabs.get(7).readResult = {origin:"https://a.test", matches:[match], truncated:false};
  const found = await f.command("page_find", {grant, tab:7, document:null, ...terms});
  assert.deepEqual(plain(found.result), {document:"doc-7", origin:"https://a.test", matches:[match], truncated:false});
  assert.equal(JSON.stringify(f.ran.at(-1)), JSON.stringify({tab:7, func:"pageInspect", args:["find", 512, terms]}));
  const ran = f.ran.length;
  for (const bad of [{role:null, name:null}, {limit:0}, {limit:51}, {text:""}, {within_ref:"row"}]) {
    const reply = await f.command("page_find", {grant, tab:7, document:null, ...terms, ...bad});
    assert.equal(reply.error, "InvalidRequest", JSON.stringify(bad));
  }
  assert.equal(f.ran.length, ran);
  f.tabs.get(7).readResult = {error:"StaleElement"};
  const stale = await f.command("page_find", {grant, tab:7, document:null, ...terms});
  assert.deepEqual(stale, {id:stale.id, ok:false, error:"StaleElement", tab:7});
});

// #11286: typing and key presses inject the one key engine into the lease's document only.
test("page_type and page_press inject pageKeys with their action", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  for (const [op, extra] of [["page_type", {ref:"1.0", text:"hi\n"}],
    ["page_press", {ref:null, key:"Enter", modifiers:["Shift","Control"]}], ["page_press", {ref:"1.0", key:"a", modifiers:[]}]]) {
    const reply = await f.command(op, {grant, tab:7, document:"doc-7", ...extra});
    assert.deepEqual(reply, {id:reply.id, ok:true, email, result:{document:"doc-7"}, tab:7}, op);
  }
  assert.deepEqual(plain(f.ran), [
    {tab:7, func:"pageKeys", args:["1.0", "type", "hi\n"]},
    {tab:7, func:"pageKeys", args:[null, "press", {key:"Enter", modifiers:["Shift","Control"]}]},
    {tab:7, func:"pageKeys", args:["1.0", "press", {key:"a", modifiers:[]}]},
  ]);
  // #11352: a maxlength cut is a success with its counts; text a handler refused is relayed, never as ok.
  f.tabs.get(7).result = {ok:true, truncated:true, typed:6, accepted:3};
  const cut = await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"abcdef"});
  assert.deepEqual(cut, {id:cut.id, ok:true, email, tab:7,
    result:{document:"doc-7", truncated:true, typed:6, accepted:3}});
  f.tabs.get(7).result = {ok:true, truncated:true, typed:6};
  assert.equal((await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"x"})).error,
    "OutcomeUnknown");
  f.tabs.get(7).result = {error:"TextDropped", typed:3, accepted:2, entered:"ab"};
  const dropped = await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"axb"});
  assert.deepEqual(dropped, {id:dropped.id, ok:false, error:"TextDropped", tab:7, typed:3, accepted:2, entered:"ab"});
  f.tabs.get(7).result = {error:"TextDropped", typed:"3", accepted:2, entered:"ab"};
  assert.equal((await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"x"})).error,
    "OutcomeUnknown");
  delete f.tabs.get(7).result;
  f.tabs.get(7).throwAfterRun = true;
  const lost = await f.command("page_press", {grant, tab:7, document:"doc-7", ref:null, key:"Tab", modifiers:[]});
  assert.deepEqual(lost, {id:lost.id, ok:false, error:"OutcomeUnknown"});
});

// #11285: page_select relays whether the value moved and refuses without a usable answer.
test("page_select relays changed and value, and its refusals", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const choose = value => f.command("page_select", {grant, tab:7, document:"doc-7", ref:"1.0", value});
  f.tabs.get(7).result = {ok:true, changed:false, value:"keep"};
  assert.deepEqual((await choose("other")).result, {document:"doc-7", changed:false, value:"keep"});
  assert.equal(JSON.stringify(f.ran.at(-1)), JSON.stringify({tab:7, func:"pageSelect", args:["1.0", "other"]}));
  for (const error of ["NotSelectable", "NoSuchOption"]) {
    f.tabs.get(7).result = {error};
    const reply = await choose("x");
    assert.deepEqual(reply, {id:reply.id, ok:false, error, tab:7});
  }
  f.tabs.get(7).result = {ok:true};
  assert.equal((await choose("x")).error, "OutcomeUnknown");
  assert.equal((await f.command("page_select", {grant, tab:7, document:"doc-7", ref:"1.0", value:5})).error, "InvalidRequest");
});

test("click drives trusted input and answers dialogs", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const click = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(click.result, {document:"doc-7"});
  assert.deepEqual(plain(f.ran), [{tab:7, func:"pageTarget", args:["1.0"]}]);
  const steps = f.cdp.map(c => c.method + (c.params?.type ? ":" + c.params.type : ""));
  assert.deepEqual(steps, ["attach", "Page.enable", "Input.dispatchMouseEvent:mouseMoved",
    "Input.dispatchMouseEvent:mousePressed", "Input.dispatchMouseEvent:mouseReleased", "Runtime.evaluate", "detach"]);
  assert.deepEqual(plain(f.cdp[3].params), {type:"mousePressed", x:10, y:20, button:"left", clickCount:1, buttons:1});
  for (const [type, policy, accepted] of [["alert","dismiss",true], ["confirm","dismiss",false], ["confirm","accept",true],
    ["prompt","dismiss",false]]) {
    f.tabs.get(7).dialog = {type, message:"Sure?"};
    f.cdp.length = 0;
    const reply = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0", dialog:policy});
    assert.deepEqual(reply.result, {document:"doc-7", dialog:{type, message:"Sure?", accepted}}, type + policy);
    assert.deepEqual(plain(f.cdp.find(c => c.method === "Page.handleJavaScriptDialog").params), {accept:accepted});
    assert.equal(f.cdp.at(-1).method, "detach");
  }
  f.tabs.get(7).dialog = null;
  const bad = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0", dialog:"maybe"});
  assert.equal(bad.error, "InvalidRequest");
  f.tabs.get(7).busy = true;
  f.cdp.length = 0;
  const busy = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(busy, {id:busy.id, ok:false, error:"DebuggerBusy", tab:7});
  assert.deepEqual(f.cdp, []);
  f.tabs.get(7).busy = false;
  f.tabs.get(7).result = {error:"Obscured"};
  const covered = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(covered, {id:covered.id, ok:false, error:"Obscured", tab:7});
  assert.equal(f.cdp.at(-1).method, "detach");
  assert.ok(!f.cdp.some(c => c.method === "Input.dispatchMouseEvent"));
});

const cdpSteps = f => f.cdp.map(c => c.method + (c.params?.type ? ":" + c.params.type : ""));

test("trusted input rechecks the document before every event", async () => {
  for (const move of [{doc:"doc-7-next"}, {origin:"https://evil.test"}]) {
    const f = await codeFixture();
    const {grant} = await connectGrant(f);
    Object.assign(f.tabs.get(7), {moveAt:"mousePressed", move});
    const reply = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"});
    assert.deepEqual(reply, {id:reply.id, ok:false, error:"DocumentChanged", tab:7, access:true,
      document:move.doc || "doc-7"}, JSON.stringify(move));
    assert.ok(!cdpSteps(f).includes("Input.dispatchMouseEvent:mouseReleased"), cdpSteps(f).join());
    assert.equal(f.cdp.at(-1).method, "detach");
  }
});

test("a no-access tab is never debugger-attached", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:true}}});
  const minted = await connectGrant(f);
  f.perms.held = false;
  const reply = await f.command("page_click", {grant:minted.grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(reply, {id:reply.id, ok:false, error:"DocumentChanged", tab:7, access:false, document:null});
  assert.deepEqual(f.cdp, []);
});

test("a failed dialog answer or a hung command answers OutcomeUnknown", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  Object.assign(f.tabs.get(7), {dialog:{type:"confirm", message:"Sure?"}, dialogFails:true});
  const failed = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0", dialog:"accept"});
  assert.deepEqual(failed, {id:failed.id, ok:false, error:"OutcomeUnknown"});
  assert.ok(!cdpSteps(f).includes("Input.dispatchMouseEvent:mouseReleased"));
  assert.equal(f.cdp.at(-1).method, "detach");
  Object.assign(f.tabs.get(7), {dialog:null, dialogFails:false, hang:"Input.dispatchMouseEvent"});
  f.cdp.length = 0;
  const pending = f.native.onMessage.fire({id:"hung", op:"page_click", email, grant, tab:7, document:"doc-7", ref:"1.0",
    dialog:"dismiss"});
  await settle();
  assert.equal(f.sent.find(m => m.id === "hung"), undefined);
  assert.notEqual(f.cdp.at(-1).method, "detach");
  const [key, bound] = [...f.timers].findLast(([, t]) => t.ms === 5000);
  f.timers.delete(key);
  bound.fn();
  await pending;
  await settle();
  assert.deepEqual(plain(f.sent.find(m => m.id === "hung")), {id:"hung", ok:false, error:"OutcomeUnknown"});
  assert.equal(f.cdp.at(-1).method, "detach");
});

// #11294: a timed-out click's leftover work must never send input through a later attachment.
test("a timed-out click sends no input after a later attach", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const t = f.tabs.get(7);
  t.hang = "Input.dispatchMouseEvent";
  const pending = f.native.onMessage.fire({id:"late", op:"page_click", email, grant, tab:7, document:"doc-7", ref:"1.0",
    dialog:"dismiss"});
  await settle();
  const [key, bound] = [...f.timers].findLast(([, x]) => x.ms === 5000);
  f.timers.delete(key);
  bound.fn();
  await pending;
  await settle();
  assert.equal(f.sent.find(m => m.id === "late").error, "OutcomeUnknown");
  const release = t.release;
  t.hang = null;
  assert.equal((await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0", dialog:"dismiss"})).ok, true);
  const before = f.cdp.length;
  release();
  await settle();
  assert.deepEqual(cdpSteps(f).slice(before), []);
});

// #11288: page_drag runs pageDrag in the lease's document and relays only document and dropped.
test("page drag runs in the lease document and needs a boolean dropped", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const drag = {grant, tab:7, document:"doc-7", ref:"1.0", target:"1.1"};
  const done = await f.command("page_drag", drag);
  assert.deepEqual(done.result, {document:"doc-7", dropped:true});
  assert.equal(JSON.stringify(f.ran.map(r => [r.func, r.args])), JSON.stringify([["pageDrag", ["1.0", "1.1"]]]));
  assert.equal((await f.command("page_drag", {...drag, target:"#drop"})).error, "InvalidRequest");
  assert.equal((await f.command("page_drag", {...drag, document:"doc-old"})).error, "DocumentChanged");
  assert.equal(f.ran.length, 1);
});

test("without the all-https permission only the clicked tab is readable", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true},
    9:{origin:"https://c.test", title:"C", active:false}}});
  const connected = await f.popup({type:"connect"});
  assert.equal(connected.ok, true);
  const {grant} = connected;
  assert.deepEqual((await f.command("page_read", {grant, tab:9, document:null, ...READ})),
    {id:"c1", ok:false, error:"PermissionNeeded", tab:9, origin:null});
  assert.equal((await f.command("page_read", {grant, tab:7, document:null, ...READ})).ok, true);
  await f.command("page_navigate", {grant, tab:7, url:"https://elsewhere.test/a/b?c=d"});
  const lost = await f.command("page_read", {grant, tab:7, document:null, ...READ});
  assert.deepEqual(lost, {id:lost.id, ok:false, error:"PermissionNeeded", tab:7, origin:"https://elsewhere.test"});
});

test("ended grants answer UnknownGrant and the badge clears with the table", async () => {
  const f = await codeFixture({held:true});
  const a = (await connectGrant(f)).grant, b = seedRow(f, "g-0000000000b0");
  await f.popup({type:"grants"});
  assert.equal(f.badge.at(-1), 2);
  assert.equal((await f.brokerEnds(a, "closed")).result, "ended");
  assert.equal((await f.command("page_tab", {grant:a, tab:7})).error, "UnknownGrant");
  assert.equal((await f.popup({type:"revoke_grant", grant:b})).revoked, 1);
  assert.equal((await f.command("page_tabs", {grant:b})).error, "UnknownGrant");
  assert.equal((await f.command("page_tabs", {grant:"g-000000000999"})).error, "UnknownGrant");
  assert.equal((await f.end("g-000000000999", false, "revoked")).result, "ended");
  assert.equal(f.badge.at(-1), 0);
  assert.deepEqual(f.ran, []);
});

test("malformed commands are refused before any page work", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const cases = [
    ["page_shout", {grant, tab:7}, "InvalidOperation"],
    ["page_read", {grant, tab:7, document:null, extra:1, ...READ}, "InvalidRequest"],
    ["page_read", {grant, tab:7}, "InvalidRequest"],
    ["page_read", {grant, tab:"7", document:null, ...READ}, "InvalidRequest"],
    ["page_click", {grant, tab:7, document:"doc-7", ref:"#go"}, "InvalidRequest"],
    ["page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:5}, "InvalidRequest"],
    // #11291: page_scroll names a ref exactly when it scrolls to an element.
    ["page_scroll", {grant, tab:7, document:"doc-7", to:"sideways", ref:null, wait_ms:0}, "InvalidRequest"],
    ["page_scroll", {grant, tab:7, document:"doc-7", to:"element", ref:null, wait_ms:0}, "InvalidRequest"],
    ["page_scroll", {grant, tab:7, document:"doc-7", to:"down", ref:"1.0", wait_ms:0}, "InvalidRequest"],
    ["page_scroll", {grant, tab:7, document:"doc-7", to:"down", ref:null, wait_ms:5001}, "InvalidRequest"],
    ["page_press", {grant, tab:7, document:"doc-7", ref:null, key:"Return", modifiers:[]}, "InvalidRequest"],
    ["page_press", {grant, tab:7, document:"doc-7", ref:null, key:"ab", modifiers:[]}, "InvalidRequest"],
    ["page_press", {grant, tab:7, document:"doc-7", ref:null, key:"\n", modifiers:[]}, "InvalidRequest"],
    ["page_press", {grant, tab:7, document:"doc-7", ref:null, key:"Enter", modifiers:["Ctrl"]}, "InvalidRequest"],
    ["page_press", {grant, tab:7, document:"doc-7", ref:null, key:"Enter", modifiers:["Alt","Alt"]}, "InvalidRequest"],
    ["page_press", {grant, tab:7, document:"doc-7", ref:"#go", key:"Enter", modifiers:[]}, "InvalidRequest"],
    ["page_press", {grant, tab:7, document:"doc-7", key:"Enter", modifiers:[]}, "InvalidRequest"],
    ["grant_end", {grant, reason:"bored", last:true}, "InvalidRequest"],
    ["grant_end", {grant, reason:"closed"}, "InvalidRequest"],
    ["grant_end", {grant, reason:"closed", last:"yes"}, "InvalidRequest"],
    ["grant_end", {grant, reason:"closed", last:true, tab:7}, "InvalidRequest"],
  ];
  for (const url of ["javascript:alert(1)", "data:text/html,x", "http://example.org/", "https://u:p@a.test/", "file:///etc", "chrome://settings"]) {
    cases.push(["page_navigate", {grant, tab:7, url}, "InvalidUrl"]);
  }
  for (const [op, fields, error] of cases) assert.equal((await f.command(op, fields)).error, error, op + JSON.stringify(fields));
  await f.native.onMessage.fire({id:"wrong", op:"page_tab", email:"other@example.org", grant, tab:7});
  await settle();
  assert.equal(f.sent.findLast(m => m.id === "wrong").error, "ProfileMismatch");
  assert.deepEqual({ran:f.ran, updates:f.updates}, {ran:[], updates:[]});
  assert.ok(f.table()[grant]);
  assert.equal(f.perms.held, true, "a refused grant_end changes nothing");
});

// #11293: a profile grant brings a tab to the front and focuses its window.
test("page_switch focuses a tab on a profile grant", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true},
    8:{origin:"https://b.test", title:"B", active:false}}});
  const {grant} = await connectGrant(f);
  const reply = await f.command("page_switch", {grant, tab:8});
  assert.deepEqual(reply, {id:reply.id, ok:true, email, result:{window:1, focused:true}, tab:8});
  assert.deepEqual(f.switches, [8]);
  assert.deepEqual(f.focused, [{win:1, focused:true}]);
  assert.equal(f.tabs.get(8).active, true);
  assert.equal(f.tabs.get(7).active, false);
  assert.equal((await f.command("page_switch", {grant, tab:99})).error, "TabNotFound");
});

test("navigate reports access, document and settled", async () => {
  const f = await codeFixture({held:false, tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true}}});
  const {grant} = await f.popup({type:"connect"});
  const same = await f.command("page_navigate", {grant, tab:7, url:"https://a.test/next"});
  assert.deepEqual(same.result, {access:true, document:"doc-7-1", origin:"https://a.test", settled:true});
  const cross = await f.command("page_navigate", {grant, tab:7, url:"https://b.test/"});
  assert.deepEqual(cross.result, {access:false, document:null, origin:null, settled:true});
  f.tabs.get(7).slow = true;
  const pending = f.native.onMessage.fire({id:"slow", op:"page_navigate", email, grant, tab:7, url:"https://b.test/x"});
  await settle();
  [...f.timers.values()].find(job => job.ms === 5000).fn();
  await pending; await settle();
  assert.equal(f.sent.findLast(m => m.id === "slow").result.settled, false);
});

test("read during an unsettled navigation never returns the old document", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true}}});
  const {grant} = await f.popup({type:"connect"});
  const t = f.tabs.get(7);
  t.slow = true;
  const nav = f.native.onMessage.fire({id:"nav", op:"page_navigate", email, grant, tab:7, url:"https://a.test/slow"});
  await settle();
  // Unsettled past the bound: a typed refusal, never the previous document.
  const read = f.native.onMessage.fire({id:"r1", op:"page_read", email, grant, tab:7, document:"doc-7", ...READ});
  await settle();
  [...f.timers.values()].find(job => job.ms === 3000)?.fn();
  await read; await settle();
  assert.deepEqual(plain(f.sent.findLast(m => m.id === "r1")), {id:"r1", ok:false, error:"NavigationPending", tab:7});
  // Settling within the bound: the read waits and returns the new document.
  const late = f.native.onMessage.fire({id:"r2", op:"page_read", email, grant, tab:7, document:"doc-7", ...READ});
  await settle();
  t.land();
  await late; await nav; await settle();
  const reply = f.sent.findLast(m => m.id === "r2");
  assert.equal(reply.ok, true);
  assert.equal(reply.result.document, "doc-7-1");
});

test("grant_end with last true removes the permission and last false keeps it", async () => {
  const f = await codeFixture();
  const a = (await connectGrant(f)).grant, b = seedRow(f, "g-0000000000b0");
  const removed = f.perms.removed;
  assert.deepEqual((await f.end(a, false, "closed")).result, "ended");
  assert.deepEqual([f.perms.held, Object.keys(f.table()), f.perms.removed], [true, [b], removed]);
  // The broker still counts a grant this worker has no row for: `last: false` keeps the permission.
  assert.deepEqual((await f.end(b, false, "revoked")).result, "ended");
  assert.deepEqual([f.perms.held, f.table(), f.perms.removed], [true, {}, removed]);
  assert.deepEqual((await f.end(b, true, "revoked")).result, "ended");
  assert.deepEqual([f.perms.held, f.table(), f.perms.removed], [false, {}, removed + 1]);
  for (const reason of ["revoked","tab_closed","browser_gone","unenrolled","audit","closed"]) {
    assert.equal((await f.end("g-000000000777", false, reason)).result, "ended", reason);
  }
});

test("a last end beside a row the broker no longer lists is settled by the broker list", async () => {
  // b's grant_end was lost: the broker list omits it, so `last` for a ends the permission.
  const f = await codeFixture();
  const a = (await connectGrant(f)).grant;
  seedRow(f, "g-0000000000b0");
  f.live.grants = [];
  await f.end(a, true);
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  // A grant the broker counts after its `last` keeps both the row and the permission.
  const g = await codeFixture();
  const x = (await connectGrant(g)).grant, y = seedRow(g, "g-0000000000b1");
  g.live.grants = g.live.grants.filter(e => e.grant === y);
  await g.end(x, true);
  assert.deepEqual([g.perms.held, Object.keys(g.table())], [true, [y]]);
});

test("the pairing acknowledgement prunes the table to its live list", async () => {
  const rows = {"g-00000000000a":{scope:"profile", tab:null}, "g-00000000000b":{scope:"profile", tab:null}};
  const f = await codeFixture({session:{grants:rows}, held:true, ack:["g-00000000000a"]});
  assert.deepEqual([f.perms.held, Object.keys(f.table())], [true, ["g-00000000000a"]]);
  assert.equal((await f.command("page_tab", {grant:"g-00000000000b", tab:7})).error, "UnknownGrant");
  // A reconnect whose list is empty ends the last row and the permission.
  f.native.onDisconnect.fire();
  await settle();
  assert.deepEqual([f.perms.held, Object.keys(f.table())], [true, ["g-00000000000a"]], "a dropped port alone changes nothing");
  [...f.timers.values()].find(job => job.ms === 250).fn();
  await settle();
  await f.handshake([]);
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  // An acknowledgement without a live list is a refused pairing: nothing is pruned or kept on its word.
  const h = await codeFixture({session:{grants:rows}, held:true, ack:false});
  const hello = h.sent.at(-1), nonce = "e".repeat(64);
  await h.native.onMessage.fire({type:"challenge", proof:sign("broker", hello, nonce), nonce});
  await h.native.onMessage.fire({ok:true, generation:"g9"});
  await settle();
  assert.equal(h.native.closed, true);
  assert.deepEqual(Object.keys(h.table()).length, 2);
});

test("an unreachable broker ends the table and the permission after the reconnect grace", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.native.onDisconnect.fire();
  await settle();
  assert.ok(Number.isFinite(f.sessionStore.offline_since));
  await f.chrome.alarms.onAlarm.fire({name:"reconnect"});
  await settle();
  assert.deepEqual([f.perms.held, Object.keys(f.table())], [true, [grant]], "the grant may still be live");
  f.sessionStore.offline_since = Date.now() - 106000;
  await f.chrome.alarms.onAlarm.fire({name:"reconnect"});
  await settle();
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  // A connected worker never ends grants on the clock.
  const g = await codeFixture();
  await connectGrant(g);
  assert.equal(g.sessionStore.offline_since, null);
  await g.chrome.alarms.onAlarm.fire({name:"reconnect"});
  await settle();
  assert.equal(g.perms.held, true);
});

test("a restarted worker keeps the permission only for a grant the broker still lists", async () => {
  const row = {"g-00000000000a":{scope:"profile", tab:null}};
  const gone = await codeFixture({session:{grants:row}, held:true});
  assert.deepEqual([gone.perms.held, gone.table()], [false, {}], "a broker with no grants clears a restored row");
  const live = [{grant:"g-00000000000a", scope:"profile", state:"active", idle_s:3}];
  const kept = await codeFixture({session:{grants:row}, held:true, live});
  assert.deepEqual([kept.perms.held, Object.keys(kept.table())], [true, ["g-00000000000a"]]);
  await kept.chrome.alarms.onAlarm.fire({name:"reconnect"});
  await settle();
  assert.deepEqual([kept.perms.held, kept.perms.removed], [true, 0]);
});

test("connect pairs an unpaired profile and records the broker's grant", async () => {
  const f = await codeFixture({local:{}});
  assert.equal(f.sent.length, 0, "no credential, no hello");
  // The broker acknowledges the hello the fresh pairing sends; connect waits for it.
  const pending = f.popup({type:"connect"});
  for (let i = 0; i < 50 && !f.sent.some(m => m.type === "hello"); i++) await settle();
  await f.handshake();
  const reply = await pending;
  const pairing = f.localStore.pairing;
  assert.deepEqual(reply, {ok:true, grant:"g-000000000001", profile:pairing.profile});
  assert.deepEqual(f.brokerCalls.map(m => m.op), ["pair", "connect"]);
  assert.deepEqual(f.brokerCalls[1], {op:"connect", profile:pairing.profile, email, key:pairing.key});
  assert.deepEqual(f.table(), {"g-000000000001":{scope:"profile", tab:null}});
  assert.ok(!JSON.stringify(reply).includes(pairing.key), "the key never leaves its frames");
  assert.ok(!f.brokerCalls.some(m => m.op === "mint"), "no code is minted");
  // A second click reuses the standing grant.
  assert.equal((await f.popup({type:"connect"})).grant, "g-000000000001");
  assert.deepEqual(await f.popup({type:"status"}), {ok:true, email, profile:pairing.profile, connected:"g-000000000001"});
});

// Owner ruling Dqw 2026-11-16: after a fresh pair, Connect retries once when the native port is still
// reconnecting. Fires the one retry wait if it was armed; reports whether it was.
const PAIR_RETRY_MS = 750;
async function fireRetry(f) {
  for (let i = 0; i < 50; i++) {
    const found = [...f.timers].find(([, job]) => job.ms === PAIR_RETRY_MS);
    if (found) { f.timers.delete(found[0]); found[1].fn(); await settle(); return true; }
    await settle();
  }
  return false;
}
const refuseConnects = (f, count) => {
  let refused = 0;
  f.setBroker(msg => msg.op === "connect" && refused++ < count ? {ok:false, error:"BrowserDisconnected"} : f.fakeBroker(msg));
  return () => f.brokerCalls.filter(m => m.op === "connect").length;
};

test("connect after lost storage re-pairs and succeeds", async () => {
  // The extension lost chrome.storage.local; the broker still holds the old pairing for this email.
  const f = await codeFixture({local:{}});
  const connects = refuseConnects(f, 1);
  const pending = f.popup({type:"connect"});
  for (let i = 0; i < 50 && !f.sent.some(m => m.type === "hello"); i++) await settle();
  await f.handshake();
  assert.equal(await fireRetry(f), true, "the retry wait was armed");
  const reply = await pending;
  const pairing = f.localStore.pairing;
  assert.deepEqual(reply, {ok:true, grant:"g-000000000001", profile:pairing.profile});
  assert.notEqual(pairing.profile, PAIRED.pairing.profile);
  assert.deepEqual(f.brokerCalls.map(m => m.op), ["pair", "connect", "connect"]);
  assert.equal(connects(), 2);
  assert.deepEqual(f.table(), {"g-000000000001":{scope:"profile", tab:null}});
});

test("connect retries exactly once and only after a fresh pair", async () => {
  const f = await codeFixture({local:{}});
  const connects = refuseConnects(f, Infinity);
  const pending = f.popup({type:"connect"});
  for (let i = 0; i < 50 && !f.sent.some(m => m.type === "hello"); i++) await settle();
  await f.handshake();
  assert.equal(await fireRetry(f), true);
  assert.equal((await pending).error, "BrowserDisconnected");
  assert.equal(connects(), 2, "one retry, never more");
  assert.equal(await fireRetry(f), false, "no second wait");
  assert.deepEqual(f.table(), {});
  // A profile that was already paired gets no retry: its port was up before the click.
  const g = await codeFixture({local:PAIRED});
  const once = refuseConnects(g, 1);
  assert.equal((await connectGrant(g)).error, "BrowserDisconnected");
  assert.equal(once(), 1);
  assert.equal(await fireRetry(g), false);
});

test("disconnect keeps the pairing and forget unpairs", async () => {
  const {profile:paired, key} = PAIRED.pairing;
  const f = await codeFixture({local:PAIRED});
  await connectGrant(f);
  assert.equal((await f.popup({type:"revoke_grants"})).ok, true);
  assert.deepEqual(f.localStore.pairing, PAIRED.pairing, "Disconnect keeps the pairing");
  assert.ok(!f.brokerCalls.some(m => m.op === "unpair"));
  // Connect again needs no new pair.
  assert.equal((await connectGrant(f)).ok, true);
  assert.equal(f.brokerCalls.filter(m => m.op === "pair").length, 0);
  // Forget this profile: the worker's unpair removes the pairing.
  assert.deepEqual(await f.popup({type:"unpair"}), {ok:true, profile:paired});
  assert.deepEqual(f.brokerCalls.at(-1), {op:"unpair", profile:paired, email, key});
  assert.equal(f.localStore.pairing, undefined);
});

test("a refusal carries the broker's reason code to the popup", async () => {
  const f = await codeFixture({local:{}});
  f.setBroker(msg => msg.op === "pair" ? {ok:false, error:"EnrollmentConflict", reason:"enroll_conflict_route"}
    : f.fakeBroker(msg));
  assert.deepEqual(await f.popup({type:"connect"}),
    {ok:false, error:"EnrollmentConflict", reason:"enroll_conflict_route"});
  f.setBroker(msg => msg.op === "pair" ? {ok:false, error:"Unauthorized", reason:"Not A Code!"} : f.fakeBroker(msg));
  assert.deepEqual(await f.popup({type:"connect"}), {ok:false, error:"Unauthorized"}, "a malformed reason is dropped");
});

test("a connect whose grant creation fails records nothing", async () => {
  const f = await codeFixture();
  for (const refusal of [{ok:false, error:"AuditUnavailable"}, "disconnect", {ok:true, grant:"bad", scope:"profile", profile},
    {ok:true, grant:"g-0000000000cc", scope:"tab", profile}]) {
    f.setBroker(msg => msg.op === "connect" ? refusal : f.fakeBroker(msg));
    assert.equal((await connectGrant(f)).ok, false, JSON.stringify(refusal));
    assert.deepEqual([f.perms.held, f.table()], [false, {}], JSON.stringify(refusal));
  }
  assert.equal((await f.popup({type:"status"})).connected, null);
  // A grant the table cannot hold is revoked again rather than left usable.
  const revokes = [];
  f.setBroker(msg => {
    if (msg.op === "revoke_grant") { revokes.push(msg.grant); return {ok:true, revoked:1}; }
    return msg.op === "connect" ? {ok:true, grant:"g-0000000000dd", scope:"profile", profile} : {ok:true, grants:[]};
  });
  f.failures.sessionSet = true;
  assert.equal((await connectGrant(f)).error, "Could not record the grant.");
  f.failures.sessionSet = false;
  assert.deepEqual(revokes, ["g-0000000000dd"]);
  // A connect with no acknowledged native port is refused before the broker is asked.
  const g = await codeFixture({ack:false});
  const waiting = g.popup({type:"connect"});
  await settle();
  [...g.timers.values()].find(job => job.ms === 5000 && job.fn.toString().includes("BrowserDisconnected")).fn();
  assert.equal((await waiting).error, "BrowserDisconnected");
  assert.ok(!g.brokerCalls.some(m => m.op === "connect"));
});

test("disconnect revokes every grant at once and drops the permission", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  assert.deepEqual(await f.popup({type:"revoke_grants"}), {ok:true, revoked:1});
  assert.deepEqual(f.brokerCalls.at(-1), {op:"revoke_grants", profile, email, key:secret});
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  assert.equal((await f.command("page_tabs", {grant})).error, "UnknownGrant");
  assert.equal((await f.popup({type:"status"})).connected, null);
});

test("status reports connected only while the broker lists a grant", async () => {
  const f = await codeFixture({local:{}});
  assert.deepEqual(await f.popup({type:"status"}), {ok:true, email, profile:null, connected:null});
  const g = await codeFixture();
  assert.equal((await g.popup({type:"status"})).connected, null);
  const {grant} = await connectGrant(g);
  assert.equal((await g.popup({type:"status"})).connected, grant);
  // A browser restart: the broker ended the grant, so the popup offers Connect again.
  g.live.grants = [];
  assert.equal((await g.popup({type:"status"})).connected, null);
  assert.deepEqual(g.table(), {});
});

test("a grants call without a credential is refused", async () => {
  const f = await codeFixture({local:{}, held:true});
  for (const msg of [{type:"grants"}, {type:"revoke_grants"}, {type:"revoke_grant", grant:"g-000000000001"}]) {
    assert.equal((await f.popup(msg)).error, "NotEnrolled", msg.type);
  }
  assert.deepEqual(f.brokerCalls, []);
  // The removed code and workflow messages are not answered at all.
  for (const type of ["mint","code_preview","activate","enroll","replace","preview","enable","unenroll"]) {
    const accepted = f.chrome.runtime.onMessage.fire({type}, {id, url:"chrome-extension://" + id + "/setup.html"}, () => assert.fail(type));
    assert.equal(accepted, undefined, type);
  }
  // A content script cannot reach the popup's messages.
  assert.equal(f.chrome.runtime.onMessage.fire({type:"connect"}, {id, url:"https://a.test/"}, () => assert.fail()), undefined);
});

test("a stale asset stamp in the acknowledgement reloads the extension once", async () => {
  const f = await codeFixture({stamp:"aaaaaaaaaaaaaaaa", ack:false});
  await f.handshake(undefined, {assets:"aaaaaaaaaaaaaaaa"});
  assert.equal(f.reloads(), 0, "the running assets match");
  const g = await codeFixture({stamp:"aaaaaaaaaaaaaaaa", ack:false});
  await g.handshake(undefined, {assets:"bbbbbbbbbbbbbbbb"});
  assert.equal(g.reloads(), 1);
  assert.deepEqual(plain(g.sessionStore), {}, "the reload cleared storage.session");
  // The reloaded worker still runs the old assets: a second mismatched ack must not reload again.
  const after = await codeFixture({stamp:"aaaaaaaaaaaaaaaa", ack:false, incarnation:null, local:g.localStore});
  await after.handshake(undefined, {assets:"bbbbbbbbbbbbbbbb"});
  assert.equal(after.reloads(), 0);
});

test("a self-reload keeps the browser incarnation and a browser restart does not", async () => {
  const g = await codeFixture({stamp:"aaaaaaaaaaaaaaaa", ack:false});
  const before = g.sent.findLast(m => m.type === "hello").incarnation;
  await g.handshake(undefined, {assets:"bbbbbbbbbbbbbbbb"});
  assert.equal(g.reloads(), 1);
  // The reloaded worker says hello with the same incarnation, so the broker rebinds the grant.
  const reloaded = await codeFixture({stamp:"bbbbbbbbbbbbbbbb", ack:false, incarnation:null, local:g.localStore});
  assert.equal(reloaded.sent.findLast(m => m.type === "hello").incarnation, before);
  assert.equal(reloaded.localStore.reload_incarnation, undefined, "the handoff is consumed once");
  // A later browser restart (storage.session empty, no handoff) gets a fresh incarnation.
  const restarted = await codeFixture({ack:false, incarnation:null, local:reloaded.localStore});
  assert.notEqual(restarted.sent.findLast(m => m.type === "hello").incarnation, before);
  // A handoff left by a reload that never restarted the worker in time is not used.
  const stale = await codeFixture({ack:false, incarnation:null,
    local:{...ENROLLED, reload_incarnation:{incarnation:before, at:Date.now() - 120000}}});
  assert.notEqual(stale.sent.findLast(m => m.type === "hello").incarnation, before);
  assert.equal(stale.localStore.reload_incarnation, undefined);
});

test("page text, typed text and URLs stay inside the operation's reply", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const read = await f.command("page_read", {grant, tab:7, document:null, ...READ});
  assert.equal(read.result.text, "private page text");
  await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"typed secret"});
  await f.command("page_navigate", {grant, tab:7, url:"https://a.test/nav-path?token=1"});
  const outside = JSON.stringify([f.sessionStore, f.brokerCalls, f.alarms, f.badge,
    f.sent.filter(m => !m.id?.startsWith?.("c"))]);
  for (const secretText of ["private page text", "typed secret", "secret-path", "nav-path", "token=1"]) {
    assert.ok(!outside.includes(secretText), secretText);
  }
  assert.deepEqual(plain(f.sessionStore.origins), {7:"https://a.test"});
});

test("a failing page script is never reported done", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).throwAfterRun = true;
  assert.equal((await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"})).error, "OutcomeUnknown");
  f.tabs.get(7).throwAfterRun = false;
  f.tabs.get(7).result = {done:"maybe"};
  assert.equal((await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"t"})).error, "OutcomeUnknown");
  f.chrome.tabs.query = async () => { throw new Error("https://a.test/private/path"); };
  const leak = await f.command("page_tabs", {grant});
  assert.deepEqual(leak, {id:leak.id, ok:false, error:"BrowserDisconnected"});
});

// #11289: file selection and downloads.
test("upload runs only in the lease's document and refuses malformed files", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const file = {name:"a.txt", type:"text/plain", data:"aGVsbG8="};
  for (const files of [[], [{...file, name:"a/b"}], [{...file, data:"%%%%"}], [{...file, extra:1}],
    Array(11).fill(file), [{...file, data:"A".repeat(700004)}]]) {
    assert.equal((await f.command("page_upload", {grant, tab:7, document:"doc-7", ref:"1.0", files})).error,
      "InvalidRequest", JSON.stringify(files).slice(0, 80));
  }
  const stale = await f.command("page_upload", {grant, tab:7, document:"doc-old", ref:"1.0", files:[file]});
  assert.equal(stale.error, "DocumentChanged");
  assert.deepEqual(f.ran, []);
  const done = await f.command("page_upload", {grant, tab:7, document:"doc-7", ref:"1.0", files:[file]});
  assert.deepEqual(done.result, {document:"doc-7"});
  assert.deepEqual(plain(f.ran), [{tab:7, func:"pageSetFiles", args:["1.0", [file]]}]);
  f.tabs.get(7).result = {error:"NotEditable"};
  const refused = await f.command("page_upload", {grant, tab:7, document:"doc-7", ref:"1.0", files:[file]});
  assert.deepEqual(refused, {id:refused.id, ok:false, error:"NotEditable", tab:7});
});

test("downloads are listed only for the tab's origin", async () => {
  const f = await codeFixture({held:true});
  const grant = seedRow(f, "g-0000000000a7", "tab", 7);
  assert.equal((await f.command("page_downloads", {grant, tab:7})).error, "InvalidOperation");
  const searches = [];
  f.chrome.downloads = {search:async query => { searches.push(query); return [
    {id:1, filename:"/dl/report.txt", url:"https://a.test/download.txt", referrer:"", mime:"text/plain",
      state:"complete", bytesReceived:32, totalBytes:32, startTime:"2026-11-15T00:00:01Z"},
    {id:2, filename:"/dl/generated.txt", url:"blob:https://a.test/0f1e", state:"in_progress",
      bytesReceived:0, totalBytes:9, startTime:"2026-11-15T00:00:00Z"},
    {id:3, filename:"/dl/other.txt", url:"https://b.test/x", referrer:"https://b.test/", state:"complete",
      bytesReceived:1, totalBytes:1, startTime:"2026-11-14T00:00:00Z"},
    {id:4, filename:"/dl/cdn.bin", url:"https://cdn.test/f", referrer:"https://a.test/page", state:"interrupted",
      error:"NETWORK_FAILED", bytesReceived:0, totalBytes:0, startTime:"2026-11-13T00:00:00Z"},
  ]; }};
  f.perms.held = true;  // the empty acknowledgement dropped it before the row was seeded
  const listed = await f.command("page_downloads", {grant, tab:7});
  assert.equal(listed.ok, true);
  assert.equal(listed.result.origin, "https://a.test");
  assert.deepEqual(listed.result.downloads.map(d => d.id), [1, 2, 4]);
  assert.deepEqual(plain(listed.result.downloads[0]), {id:1, path:"/dl/report.txt", url:"https://a.test/download.txt",
    mime:"text/plain", state:"complete", bytes:32, total:32, error:null, started:"2026-11-15T00:00:01Z"});
  assert.equal(listed.result.downloads[2].error, "NETWORK_FAILED");
  assert.equal((await f.command("page_downloads", {grant, tab:8})).error, "OutOfScope");
  assert.equal(searches.length, 1);
});

test("a paired profile connects, lists and says hello with its pairing credential", async () => {
  const {profile:paired, key} = PAIRED.pairing;
  const f = await codeFixture({local:PAIRED});
  assert.deepEqual([f.sent[0].type, f.sent[0].profile], ["hello", paired]);
  assert.equal(f.native.closed, false);
  const connected = await connectGrant(f);
  assert.equal(connected.ok, true);
  assert.deepEqual(f.brokerCalls.at(-1), {op:"connect", profile:paired, email, key});
  assert.equal((await f.popup({type:"grants"})).grants[0].grant, connected.grant);
  assert.deepEqual(f.brokerCalls.at(-1), {op:"grants", profile:paired, email, key});
  assert.ok(!JSON.stringify([connected, f.sessionStore]).includes(key), "the key never leaves its frames");
});

test("pair records the pairing only on ok and retries with the same fields", async () => {
  const f = await codeFixture({local:{}, held:true});
  f.setBroker(msg => msg.op === "pair" ? "disconnect" : f.fakeBroker(msg));
  const first = await f.popup({type:"pair"});
  assert.equal(first.ok, false);
  const pending = f.localStore.pairing_pending;
  assert.match(pending.profile, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.match(pending.key, /^[0-9a-f]{64}$/);
  assert.deepEqual([f.localStore.pairing, f.perms.held, f.sent.length], [undefined, false, 0],
    "a failed pair leaves no pairing, no permission and no connection");
  f.setBroker(f.fakeBroker);
  const done = await f.popup({type:"pair"});
  assert.deepEqual(done, {ok:true, profile:pending.profile});
  assert.deepEqual(f.brokerCalls.filter(m => m.op === "pair"), [{op:"pair", ...pending}, {op:"pair", ...pending}]);
  assert.deepEqual([f.localStore.pairing, f.localStore.pairing_pending], [pending, undefined]);
  assert.deepEqual([f.sent.at(-1).type, f.sent.at(-1).profile], ["hello", pending.profile], "pairing reconnects the port");
  assert.ok(!JSON.stringify([first, done]).includes(pending.key));
  // A definite refusal drops the pending fields so the next Connect click starts fresh.
  const g = await codeFixture({local:{}});
  g.setBroker(msg => msg.op === "pair" ? {ok:false, error:"EnrollmentConflict"} : g.fakeBroker(msg));
  assert.equal((await g.popup({type:"connect"})).error, "EnrollmentConflict");
  assert.deepEqual([g.localStore.pairing, g.localStore.pairing_pending], [undefined, undefined]);
  assert.ok(!g.brokerCalls.some(m => m.op === "connect"));
  // A workflow enrollment is never paired over; it connects with its own credential.
  const h = await codeFixture();
  assert.match((await h.popup({type:"pair"})).error, /workflow enrollment/);
  assert.ok(!h.brokerCalls.some(m => m.op === "pair"));
});

test("unpair removes the permission and the pairing record", async () => {
  const {profile:paired, key} = PAIRED.pairing;
  const f = await codeFixture({local:PAIRED});
  await connectGrant(f);
  const hellos = f.sent.filter(m => m.type === "hello").length;
  assert.deepEqual(await f.popup({type:"unpair"}), {ok:true, profile:paired});
  assert.deepEqual(f.brokerCalls.at(-1), {op:"unpair", profile:paired, email, key});
  assert.deepEqual([f.perms.held, f.table(), f.localStore.pairing], [false, {}, undefined]);
  assert.equal(f.native.closed, true);
  assert.equal(f.sent.filter(m => m.type === "hello").length, hellos, "no credential, no new hello");
  // A retry after a lost reply treats Unauthorized as done; a first Unauthorized is a refusal.
  const g = await codeFixture({local:PAIRED});
  await connectGrant(g);
  g.setBroker(msg => msg.op === "unpair" ? "disconnect" : g.fakeBroker(msg));
  assert.equal((await g.popup({type:"unpair"})).ok, false);
  assert.equal(g.localStore.pairing.unpairing, true);
  assert.equal(g.perms.held, true, "a pairing that still stands keeps its grants' permission");
  g.setBroker(msg => msg.op === "unpair" ? {ok:false, error:"Unauthorized"} : g.fakeBroker(msg));
  assert.equal((await g.popup({type:"unpair"})).ok, true);
  assert.deepEqual([g.perms.held, g.table(), g.localStore.pairing], [false, {}, undefined]);
  const h = await codeFixture({local:PAIRED});
  h.setBroker(msg => msg.op === "unpair" ? {ok:false, error:"Unauthorized"} : h.fakeBroker(msg));
  assert.equal((await h.popup({type:"unpair"})).error, "Unauthorized");
  assert.equal(h.localStore.pairing.profile, paired);
});

test("back, forward and reload report where the tab landed", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true},
    9:{origin:"https://c.test", title:"C", active:false}}});
  const {grant} = await connectGrant(f);
  assert.deepEqual(await f.command("page_back", {grant, tab:7}), {id:"c1", ok:false, error:"NoHistory", tab:7});
  await f.command("page_navigate", {grant, tab:7, url:"https://a.test/second"});
  const back = await f.command("page_back", {grant, tab:7});
  assert.deepEqual(back.result, {access:true, document:"doc-7-2", origin:"https://a.test", settled:true});
  const reload = await f.command("page_reload", {grant, tab:7});
  assert.deepEqual(reload.result, {access:true, document:"doc-7-3", origin:"https://a.test", settled:true});
  assert.equal(JSON.stringify(f.tabs.get(7).reloads), JSON.stringify([{bypassCache:false}]));
  assert.equal((await f.command("page_forward", {grant, tab:7})).result.document, "doc-7-4");
  assert.equal((await f.command("page_forward", {grant, tab:7})).error, "NoHistory");
  for (const op of ["page_back", "page_forward", "page_reload"]) {
    assert.equal((await f.command(op, {grant:"g-000000000999", tab:9})).error, "UnknownGrant");
    assert.equal((await f.command(op, {grant, tab:7, url:"https://a.test/"})).error, "InvalidRequest");
  }
  assert.equal(f.tabs.get(9).past, undefined);
  assert.equal(f.tabs.get(9).reloads, undefined);
});
