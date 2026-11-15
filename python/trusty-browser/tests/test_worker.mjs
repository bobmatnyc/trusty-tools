import assert from "node:assert/strict";
import {createHmac, webcrypto} from "node:crypto";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../src/trusty_browser/extension/worker.js",import.meta.url),"utf8");
const pagesSource = await readFile(new URL("../src/trusty_browser/extension/pages.js",import.meta.url),"utf8");
const codesSource = await readFile(new URL("../src/trusty_browser/extension/codes.js",import.meta.url),"utf8");
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
  ctx.importScripts = (...files) => { vm.runInContext(pagesSource,ctx); vm.runInContext(codesSource,ctx); };
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

test("valid role-bound proofs admit a pinned top-level document and recheck Chrome email", async () => {
  const f = await fixture();
  const h=f.sent[0], nonce="b".repeat(64);
  await f.port.onMessage.fire({type:"challenge",proof:sign("broker",h,nonce),nonce});
  assert.equal(f.sent[1].proof,sign("extension",h,nonce));
  await f.port.onMessage.fire({ok:true,generation:"g1",live:[]});
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
const CODE = "tb-7K3M9Q-1ZXD4R-H8V2NB-QW6TPA";
const ENROLLED = {enrollment:{profile, key:secret, email, workflow:"w", url:route, account:"a"}};
const PAIRED = {pairing:{profile:"5f0c6b1e-2d4a-4c8e-9b7a-1e2f3a4b5c6d", email, key:"b".repeat(64)}};

// A browser with explicit tabs, a fake broker behind both native hosts, and the real worker table.
async function codeFixture({session:seed = {}, held = false, tabs:layout, live = [], local:localSeed = ENROLLED,
  ack} = {}) {
  const sent = [], ran = [], queries = [], updates = [], brokerCalls = [], alarms = [], badge = [], cdp = [];
  const timers = new Map(); let serial = 0, mintSerial = 0, generation = 0;
  const tabs = new Map(Object.entries(layout || {
    7:{origin:"https://a.test", title:"Alpha", active:true},
    8:{origin:"https://b.test", title:"Beta", active:false},
    9:{origin:"https://c.test", title:"Gamma", active:false},
  }).map(([id, t]) => [Number(id), {doc:"doc-" + id, ...t}]));
  const perms = {held, removed:0, requested:0};
  const sessionStore = {incarnation:"epoch-1", ...structuredClone(seed)};
  const localStore = structuredClone(localSeed);
  const failures = {sessionSet:false};
  const listeners = new Set();
  const ok = {grants:[...live]};
  const fakeBroker = msg => {
    if (msg.op === "mint") {
      const grant = "g-" + String(++mintSerial).padStart(12, "0");
      ok.grants.push({grant, scope:msg.scope, ...(msg.scope === "tab" ? {tab:msg.tab} : {}), state:"code", expires_in_s:300});
      return {ok:true, code:CODE, grant, scope:msg.scope, profile:msg.profile, ...(msg.scope === "tab" ? {tab:msg.tab} : {}),
        expires_at:Date.now() / 1000 + 300};
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
  const accessible = t => t && (t.activeTab || (perms.held && t.origin.startsWith("https:")));
  const native = {onMessage:event(), onDisconnect:event(), postMessage(m){sent.push(m);}, disconnect(){this.closed = true;}};
  const chrome = {
    runtime:{id, onConnect:event(), onMessage:event(), onStartup:event(), onInstalled:event(),
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
    tabs:{
      get:async tab => { if (!tabs.has(tab)) throw new Error("No tab with id: " + tab); return {id:tab}; },
      query:async q => { queries.push(q); return [...tabs].map(([tid, t], index) => ({id:tid, windowId:1, index, active:t.active})); },
      update:async (tab, {url}) => {
        const t = tabs.get(tab);
        if (!t) throw new Error("No tab");
        updates.push({tab, url});
        const next = new URL(url).origin;
        if (next !== t.origin) t.activeTab = false;
        t.origin = next; t.doc = "doc-" + tab + "-" + updates.length;
        if (!t.slow) setImmediate(() => { for (const l of [...listeners]) l(tab, {status:"loading"}); for (const l of [...listeners]) l(tab, {status:"complete"}); });
        return {id:tab};
      },
      onUpdated:{addListener(l){listeners.add(l);}, removeListener(l){listeners.delete(l);}},
    },
    scripting:{executeScript:async ({target, func, args}) => {
      const t = tabs.get(target.tabId);
      if (!accessible(t)) throw new Error("Cannot access contents of the page");
      if (target.documentIds && !target.documentIds.includes(t.doc)) throw new Error("No document with id");
      if (func.name !== "pageProbe") ran.push({tab:target.tabId, func:func.name, args});
      const result = func.name === "pageProbe" ? [t.origin, t.title]
        : func.name === "pageRead" ? {origin:t.origin, url:t.origin + "/secret-path?q=1", title:t.title,
          text:"private page text", elements:[{ref:"1.0", tag:"button", role:"button", name:"Go"}], truncated:false}
        : t.result || {ok:true, x:10, y:20};
      if (t.throwAfterRun && func.name !== "pageProbe") throw new Error("script error");
      return [{frameId:0, documentId:t.doc, result}];
    }},
    // #11294: CDP input. `t.dialog` opens one native dialog at the mouse press (`t.dialogFails` makes its
    // answer fail); `t.busy` refuses attach; `t.moveAt` names an event type after which the tab gets
    // `t.move` ({doc} or {origin}); `t.hang` names a method that never answers. Like Chrome, attach needs
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
        if (t.hang === method) return new Promise(() => {});
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
  ctx.importScripts = () => { vm.runInContext(pagesSource, ctx); vm.runInContext(codesSource, ctx); };
  vm.runInContext(source, ctx);
  await settle();
  // The broker's pairing acknowledgement; `liveIds` defaults to everything the fake broker lists.
  const handshake = async liveIds => {
    const hello = sent.findLast(m => m.type === "hello"), nonce = "d".repeat(64);
    const key = (localStore.enrollment || localStore.pairing).key;
    await native.onMessage.fire({type:"challenge", proof:sign("broker", hello, nonce, key), nonce});
    await native.onMessage.fire({ok:true, generation:"g" + ++generation, live:liveIds ?? ok.grants.map(g => g.grant)});
    await settle();
  };
  if (ack !== false && sent.length) await handshake(ack);
  let seq = 0;
  const command = async (op, fields = {}) => {
    const id = "c" + ++seq;
    // The broker always sends a dialog policy with a click or type (#11294).
    if ((op === "page_click" || op === "page_type") && !("dialog" in fields)) fields = {...fields, dialog:"dismiss"};
    await native.onMessage.fire({id, op, email, ...fields});
    await settle();
    return plain(sent.findLast(m => m.id === id));
  };
  const popup = msg => new Promise(resolve =>
    chrome.runtime.onMessage.fire(msg, {id, url:"chrome-extension://" + id + "/setup.html"}, resolve)).then(async reply => {
    await settle();
    return plain(reply);
  });
  const table = () => plain(sessionStore.grants || {});
  return {ctx, chrome, native, sent, ran, cdp, queries, updates, brokerCalls, alarms, badge, timers, tabs, perms, failures,
    sessionStore, localStore, command, popup, table, handshake, live:ok, fakeBroker,
    setBroker(fn){ broker = fn; },
    end:(grant, last, reason = "closed") => command("grant_end", {grant, reason, last}),
    // The broker's own end: it drops the grant from its list, then sends grant_end with `last`.
    brokerEnds(grant, reason = "revoked") {
      ok.grants = ok.grants.filter(g => g.grant !== grant);
      return command("grant_end", {grant, reason, last:ok.grants.length === 0});
    }};
}
const mintTab = async (f, tab) => {
  await f.chrome.permissions.request({origins:["https://*/*"]});
  return f.popup({type:"mint", scope:"tab", tab});
};

test("a tab grant never reaches another tab", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true},
    8:{origin:"https://b.test", title:"B", active:false}}});
  const {grant} = await mintTab(f, 7);
  assert.equal(f.perms.held, true);
  const before = {ran:f.ran.length, updates:f.updates.length};
  for (const [op, extra] of [["page_tab",{}], ["page_read",{document:null}], ["page_click",{document:"doc-8", ref:"1.0"}],
    ["page_type",{document:"doc-8", ref:"1.0", text:"x"}], ["page_navigate",{url:"https://b.test/next"}]]) {
    const reply = await f.command(op, {grant, tab:8, ...extra});
    assert.deepEqual(reply, {id:reply.id, ok:false, error:"OutOfScope", tab:8}, op);
  }
  assert.deepEqual({ran:f.ran.length, updates:f.updates.length}, before);
  // The redeem probe names the granted tab and the grant explicitly.
  const probe = await f.command("page_tab", {grant, tab:7});
  assert.deepEqual(probe, {id:probe.id, ok:true, email, tab:7,
    result:{access:true, document:"doc-7", origin:"https://a.test", title:"A"}});
  const listed = await f.command("page_tabs", {grant});
  assert.deepEqual(listed.result.tabs.map(t => t.tab), [7]);
  assert.equal((await f.command("page_read", {grant, tab:7, document:null})).ok, true);
  assert.ok(f.queries.every(q => !("active" in q)), JSON.stringify(f.queries));
});

test("stale document refuses before the page runs", async () => {
  const f = await codeFixture();
  const {grant} = await mintTab(f, 7);
  for (const [op, extra] of [["page_click",{ref:"1.0"}], ["page_type",{ref:"1.0", text:"secret typed"}]]) {
    for (const document of ["doc-old", null]) {
      const reply = await f.command(op, {grant, tab:7, document, ...extra});
      assert.deepEqual(reply, {id:reply.id, ok:false, error:"DocumentChanged", tab:7, access:true, document:"doc-7"}, op);
    }
  }
  assert.deepEqual(f.ran, []);
  const done = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(done.result, {document:"doc-7"});
  assert.equal(f.ran.length, 1);
});

test("scroll runs in the lease's document and relays its view", async () => {
  // #11291
  const f = await codeFixture();
  const {grant} = await mintTab(f, 7);
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

test("click and type drive trusted input and answer dialogs", async () => {
  const f = await codeFixture();
  const {grant} = await mintTab(f, 7);
  const click = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(click.result, {document:"doc-7"});
  assert.deepEqual(plain(f.ran), [{tab:7, func:"pageTarget", args:["1.0", "click"]}]);
  const steps = f.cdp.map(c => c.method + (c.params?.type ? ":" + c.params.type : ""));
  assert.deepEqual(steps, ["attach", "Page.enable", "Input.dispatchMouseEvent:mouseMoved",
    "Input.dispatchMouseEvent:mousePressed", "Input.dispatchMouseEvent:mouseReleased", "Runtime.evaluate", "detach"]);
  assert.deepEqual(plain(f.cdp[3].params), {type:"mousePressed", x:10, y:20, button:"left", clickCount:1, buttons:1});
  f.cdp.length = 0;
  await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"a\n"});
  assert.deepEqual(f.cdp.filter(c => c.method === "Input.dispatchKeyEvent").map(c => [c.params.type, c.params.key]),
    [["keyDown","a"], ["keyUp","a"], ["keyDown","Enter"], ["keyUp","Enter"]]);
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
    const {grant} = await mintTab(f, 7);
    Object.assign(f.tabs.get(7), {moveAt:"mousePressed", move});
    const reply = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"});
    assert.deepEqual(reply, {id:reply.id, ok:false, error:"DocumentChanged", tab:7, access:true,
      document:move.doc || "doc-7"}, JSON.stringify(move));
    assert.ok(!cdpSteps(f).includes("Input.dispatchMouseEvent:mouseReleased"), cdpSteps(f).join());
    assert.equal(f.cdp.at(-1).method, "detach");
    // Typing stops at the keystroke after the move.
    Object.assign(f.tabs.get(7), {doc:"doc-7", origin:"https://a.test", moveAt:"keyUp"});
    f.cdp.length = 0;
    const typed = await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"abc"});
    assert.equal(typed.error, "DocumentChanged");
    assert.deepEqual(f.cdp.filter(c => c.params?.type === "keyDown").map(c => c.params.key), ["a"]);
    assert.equal(f.cdp.at(-1).method, "detach");
  }
});

test("a no-access tab is never debugger-attached", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:true}}});
  const minted = await f.popup({type:"mint", scope:"profile"});
  f.perms.held = false;
  const reply = await f.command("page_click", {grant:minted.grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(reply, {id:reply.id, ok:false, error:"DocumentChanged", tab:7, access:false, document:null});
  assert.deepEqual(f.cdp, []);
});

test("a failed dialog answer or a hung command answers OutcomeUnknown", async () => {
  const f = await codeFixture();
  const {grant} = await mintTab(f, 7);
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

test("ctrl or meta key events never reach the tab", async () => {
  const f = await codeFixture();
  const {grant} = await mintTab(f, 7);
  const {trusted} = f.ctx.TrustyPages, type = trusted.type;
  for (const modifiers of [2, 4]) {
    for (const key of ["w", "t", "l"]) {
      assert.equal(trusted.keyAllowed("Input.dispatchKeyEvent", {type:"keyDown", key, modifiers}), false, key + modifiers);
      trusted.type = send => send("Input.dispatchKeyEvent", {type:"keyDown", key, code:"Key" + key.toUpperCase(), modifiers});
      f.cdp.length = 0;
      const reply = await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:key});
      assert.deepEqual(reply, {id:reply.id, ok:false, error:"InvalidRequest"}, key + modifiers);
      assert.ok(!f.cdp.some(c => c.method === "Input.dispatchKeyEvent"));
      assert.equal(f.cdp.at(-1).method, "detach");
    }
  }
  trusted.type = type;
  assert.equal(trusted.keyAllowed("Input.dispatchKeyEvent", {type:"keyDown", key:"w"}), true);
});

test("without the all-https permission only the clicked tab is readable", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true},
    9:{origin:"https://c.test", title:"C", active:false}}});
  const minted = await f.popup({type:"mint", scope:"profile"});
  assert.equal(minted.ok, true);
  const {grant} = minted;
  assert.deepEqual((await f.command("page_read", {grant, tab:9, document:null})),
    {id:"c1", ok:false, error:"PermissionNeeded", tab:9, origin:null});
  assert.equal((await f.command("page_read", {grant, tab:7, document:null})).ok, true);
  await f.command("page_navigate", {grant, tab:7, url:"https://elsewhere.test/a/b?c=d"});
  const lost = await f.command("page_read", {grant, tab:7, document:null});
  assert.deepEqual(lost, {id:lost.id, ok:false, error:"PermissionNeeded", tab:7, origin:"https://elsewhere.test"});
  const mints = f.brokerCalls.filter(m => m.op === "mint").length;
  assert.equal((await f.popup({type:"mint", scope:"tab", tab:9})).error, "PermissionNeeded");
  assert.equal(f.brokerCalls.filter(m => m.op === "mint").length, mints);
});

test("ended grants answer UnknownGrant and the badge clears with the table", async () => {
  const f = await codeFixture();
  const a = (await mintTab(f, 7)).grant, b = (await f.popup({type:"mint", scope:"profile"})).grant;
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
  const {grant} = await mintTab(f, 7);
  const cases = [
    ["page_shout", {grant, tab:7}, "InvalidOperation"],
    ["page_read", {grant, tab:7, document:null, extra:1}, "InvalidRequest"],
    ["page_read", {grant, tab:7}, "InvalidRequest"],
    ["page_read", {grant, tab:"7", document:null}, "InvalidRequest"],
    ["page_click", {grant, tab:7, document:"doc-7", ref:"#go"}, "InvalidRequest"],
    ["page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:5}, "InvalidRequest"],
    // #11291: page_scroll names a ref exactly when it scrolls to an element.
    ["page_scroll", {grant, tab:7, document:"doc-7", to:"sideways", ref:null, wait_ms:0}, "InvalidRequest"],
    ["page_scroll", {grant, tab:7, document:"doc-7", to:"element", ref:null, wait_ms:0}, "InvalidRequest"],
    ["page_scroll", {grant, tab:7, document:"doc-7", to:"down", ref:"1.0", wait_ms:0}, "InvalidRequest"],
    ["page_scroll", {grant, tab:7, document:"doc-7", to:"down", ref:null, wait_ms:5001}, "InvalidRequest"],
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

test("navigate reports access, document and settled", async () => {
  const f = await codeFixture({held:false, tabs:{7:{origin:"https://a.test", title:"A", active:true, activeTab:true}}});
  const {grant} = await f.popup({type:"mint", scope:"tab", tab:7});
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

test("grant_end with last true removes the permission and last false keeps it", async () => {
  const f = await codeFixture();
  const a = (await mintTab(f, 7)).grant, b = (await f.popup({type:"mint", scope:"profile"})).grant;
  const removed = f.perms.removed;
  assert.deepEqual((await f.end(a, false, "idle")).result, "ended");
  assert.deepEqual([f.perms.held, Object.keys(f.table()), f.perms.removed], [true, [b], removed]);
  // The broker still counts a code this worker has no row for: `last: false` keeps the permission.
  assert.deepEqual((await f.end(b, false, "expired")).result, "ended");
  assert.deepEqual([f.perms.held, f.table(), f.perms.removed], [true, {}, removed]);
  assert.deepEqual((await f.end(b, true, "expired")).result, "ended");
  assert.deepEqual([f.perms.held, f.table(), f.perms.removed], [false, {}, removed + 1]);
  // Every end reason is accepted; a popup revoke or revoke-all leaves the permission to the broker's end.
  for (const reason of ["revoked","tab_closed","browser_gone","unenrolled","idle","audit","closed","expired"]) {
    assert.equal((await f.end("g-000000000777", false, reason)).result, "ended", reason);
  }
  const c = (await mintTab(f, 7)).grant;
  await f.popup({type:"revoke_grants"});
  assert.deepEqual([f.perms.held, f.table()], [true, {}]);
  await f.end(c, true, "revoked");
  assert.equal(f.perms.held, false);
  assert.ok(!f.alarms.some(alarm => alarm.name.startsWith("code-expiry:")), "no worker-side expiry poll");
});

test("a last end beside a row the broker no longer lists is settled by the broker list", async () => {
  // b's grant_end was lost: the broker list omits it, so `last` for a ends the permission.
  const f = await codeFixture();
  const a = (await mintTab(f, 7)).grant, b = (await f.popup({type:"mint", scope:"profile"})).grant;
  f.live.grants = [];
  await f.end(a, true);
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  // A mint the broker counts after its `last` keeps both the row and the permission.
  const g = await codeFixture();
  const x = (await mintTab(g, 7)).grant, y = (await g.popup({type:"mint", scope:"profile"})).grant;
  g.live.grants = g.live.grants.filter(e => e.grant === y);
  await g.end(x, true);
  assert.deepEqual([g.perms.held, Object.keys(g.table())], [true, [y]]);
  assert.ok(b);
});

test("the pairing acknowledgement prunes the table to its live list", async () => {
  const rows = {"g-00000000000a":{scope:"profile", tab:null}, "g-00000000000b":{scope:"tab", tab:7}};
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
  // A mint recorded after the hello survives an acknowledgement computed before it.
  const g = await codeFixture({held:true, ack:false});
  const fresh = (await g.popup({type:"mint", scope:"profile"})).grant;
  await g.handshake([]);
  assert.deepEqual([g.perms.held, Object.keys(g.table())], [true, [fresh]]);
  // An acknowledgement without a live list is a refused pairing: nothing is pruned or kept on its word.
  const h = await codeFixture({session:{grants:rows}, held:true, ack:false});
  const hello = h.sent.at(-1), nonce = "e".repeat(64);
  await h.native.onMessage.fire({type:"challenge", proof:sign("broker", hello, nonce), nonce});
  await h.native.onMessage.fire({ok:true, generation:"g9"});
  await settle();
  assert.equal(h.native.closed, true);
  assert.deepEqual(Object.keys(h.table()).length, 2);
});

test("an unreachable broker ends the table and the permission after the code lifetime", async () => {
  const f = await codeFixture();
  const {grant} = await f.popup({type:"mint", scope:"profile"});
  await f.chrome.permissions.request();
  f.native.onDisconnect.fire();
  await settle();
  assert.ok(Number.isFinite(f.sessionStore.offline_since));
  await f.chrome.alarms.onAlarm.fire({name:"reconnect"});
  await settle();
  assert.deepEqual([f.perms.held, Object.keys(f.table())], [true, [grant]], "a code may still be live");
  f.sessionStore.offline_since = Date.now() - 361000;
  await f.chrome.alarms.onAlarm.fire({name:"reconnect"});
  await settle();
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  // A connected worker never ends grants on the clock.
  const g = await codeFixture();
  await mintTab(g, 7);
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

test("failure branches never leave a grant usable or a permission held", async () => {
  const f = await codeFixture();
  f.setBroker(msg => msg.op === "mint" ? {ok:false, error:"AuditUnavailable"} : {ok:true, grants:[]});
  assert.equal((await mintTab(f, 7)).error, "AuditUnavailable");
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  f.setBroker(msg => msg.op === "mint" ? "disconnect" : {ok:true, grants:[]});
  assert.equal((await mintTab(f, 7)).ok, false);
  assert.equal(f.perms.held, false);
  // A failed mint beside a live grant keeps the permission: the broker list still names it.
  const kept = {grant:"g-0000000000ab", scope:"profile", state:"active", idle_s:1};
  f.setBroker(msg => msg.op === "mint" ? {ok:false, error:"AuditUnavailable"} : {ok:true, grants:[kept]});
  assert.equal((await mintTab(f, 7)).error, "AuditUnavailable");
  assert.equal(f.perms.held, true);
  const revokes = [];
  f.setBroker(msg => {
    if (msg.op === "revoke_grant") { revokes.push(msg.grant); return {ok:true, revoked:1}; }
    return msg.op === "mint" ? {ok:true, code:"not-a-code", grant:"g-0000000000bb", scope:"tab", tab:7, expires_at:1} : {ok:true, grants:[]};
  });
  assert.equal((await mintTab(f, 7)).error, "Invalid mint reply");
  assert.equal(f.perms.held, false);
  f.setBroker(msg => {
    if (msg.op === "revoke_grant") { revokes.push(msg.grant); return {ok:true, revoked:1}; }
    return msg.op === "mint" ? {ok:true, code:CODE, grant:"g-0000000000cc", scope:"tab", profile, tab:7, expires_at:1e10} : {ok:true, grants:[]};
  });
  f.failures.sessionSet = true;
  assert.equal((await mintTab(f, 7)).error, "Could not record the grant.");
  f.failures.sessionSet = false;
  assert.deepEqual(revokes, ["g-0000000000bb", "g-0000000000cc"]);
  assert.deepEqual([f.perms.held, f.table()], [false, {}]);
  // A page script that fails while its document is current, or answers oddly, is never reported done.
  f.setBroker(msg => msg.op === "mint" ? {ok:true, code:CODE, grant:"g-0000000000dd", scope:"tab", profile, tab:7, expires_at:1e10} : {ok:true, grants:[]});
  const {grant} = await mintTab(f, 7);
  f.tabs.get(7).throwAfterRun = true;
  assert.equal((await f.command("page_click", {grant, tab:7, document:"doc-7", ref:"1.0"})).error, "OutcomeUnknown");
  f.tabs.get(7).throwAfterRun = false;
  f.tabs.get(7).result = {done:"maybe"};
  assert.equal((await f.command("page_type", {grant, tab:7, document:"doc-7", ref:"1.0", text:"t"})).error, "OutcomeUnknown");
  f.chrome.tabs.get = async () => { throw new Error("https://a.test/private/path"); };
  const unexpected = await f.command("page_tabs", {grant});
  assert.equal(unexpected.error, "TabNotFound");
  f.chrome.tabs.query = async () => { throw new Error("https://a.test/private/path"); };
  f.setBroker(msg => msg.op === "mint" ? {ok:true, code:CODE, grant:"g-0000000000ee", scope:"profile", profile, expires_at:1e10} : {ok:true, grants:[]});
  const profileGrant = (await f.popup({type:"mint", scope:"profile"})).grant;
  assert.equal(profileGrant, "g-0000000000ee");
  const leak = await f.command("page_tabs", {grant:profileGrant});
  assert.deepEqual(leak, {id:leak.id, ok:false, error:"BrowserDisconnected"});
});

test("a failed mint whose grants call also fails releases an unused permission", async () => {
  const f = await codeFixture();
  // Contract "All-https access", signal 6: the native port stays up, so neither the offline check nor an
  // acknowledgement would settle this; an empty table means nothing live can use the permission.
  for (const refusal of [{ok:false, error:"AuditUnavailable"}, "disconnect"]) {
    f.setBroker(msg => msg.op === "mint" ? {ok:false, error:"AuditUnavailable"} : refusal);
    assert.equal((await mintTab(f, 7)).error, "AuditUnavailable");
    assert.notEqual(f.native.closed, true);
    assert.deepEqual([f.perms.held, f.table()], [false, {}], JSON.stringify(refusal));
  }
  // A live row in the table keeps the permission and the row; its own grant_end settles it later.
  f.setBroker(f.fakeBroker);
  const {grant} = await mintTab(f, 7);
  f.setBroker(() => ({ok:false, error:"AuditUnavailable"}));
  assert.equal((await mintTab(f, 8)).error, "AuditUnavailable");
  assert.deepEqual([f.perms.held, Object.keys(f.table())], [true, [grant]]);
});

test("page text, typed text and URLs stay inside the operation's reply", async () => {
  const f = await codeFixture();
  const {grant} = await mintTab(f, 7);
  const read = await f.command("page_read", {grant, tab:7, document:null});
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

test("code preview probes the tab the popup names and never the active tab", async () => {
  const f = await codeFixture({tabs:{7:{origin:"https://a.test", title:"A", active:false, activeTab:true},
    8:{origin:"https://b.test", title:"B", active:true}}});
  const preview = await f.popup({type:"code_preview", tab:7});
  assert.deepEqual(preview, {ok:true, paired:false, email, profile,
    target:{tab:7, access:true, origin:"https://a.test", title:"A"}});
  assert.deepEqual((await f.popup({type:"code_preview", tab:8})).target.access, false);
  assert.equal((await f.popup({type:"code_preview", tab:"7"})).error, "InvalidRequest");
  assert.deepEqual(f.queries, []);
  assert.ok(!f.brokerCalls.some(m => m.op === "mint"));
  const minted = await f.popup({type:"mint", scope:"tab", tab:7});
  assert.deepEqual(plain(f.brokerCalls.at(-1)), {op:"mint", profile, email, key:secret, scope:"tab", tab:7});
  assert.equal(JSON.stringify(f.sessionStore).includes(minted.code), false);
});

test("a mint without a pairing is refused", async () => {
  const f = await codeFixture({local:{}, held:true});
  assert.equal(f.sent.length, 0, "no credential, no hello");
  for (const msg of [{type:"mint", scope:"profile"}, {type:"mint", scope:"tab", tab:7}, {type:"grants"},
    {type:"revoke_grants"}, {type:"revoke_grant", grant:"g-000000000001"}]) {
    assert.equal((await f.popup(msg)).error, "NotEnrolled", msg.type);
  }
  assert.deepEqual(f.brokerCalls, []);
  assert.deepEqual(await f.popup({type:"code_preview", tab:null}), {ok:true, paired:false, email, profile:null, target:null});
});

test("a paired profile mints, lists and connects with its pairing credential", async () => {
  const {profile:paired, key} = PAIRED.pairing;
  const f = await codeFixture({local:PAIRED});
  assert.deepEqual([f.sent[0].type, f.sent[0].profile], ["hello", paired]);
  assert.equal(f.native.closed, false);
  const preview = await f.popup({type:"code_preview", tab:null});
  assert.deepEqual(preview, {ok:true, paired:true, email, profile:paired, target:null});
  const minted = await mintTab(f, 7);
  assert.equal(minted.ok, true);
  assert.deepEqual(f.brokerCalls.at(-1), {op:"mint", profile:paired, email, key, scope:"tab", tab:7});
  assert.equal((await f.popup({type:"grants"})).grants[0].grant, minted.grant);
  assert.deepEqual(f.brokerCalls.at(-1), {op:"grants", profile:paired, email, key});
  // The workflow controls stay closed while the pairing exists.
  for (const type of ["activate","enroll","replace"]) {
    assert.match((await f.popup({type, route:{}, workflow:"w"})).error, /Disconnect it first/);
  }
  assert.ok(!f.brokerCalls.some(m => ["enroll","activate","replace","resolve"].includes(m.op)));
  assert.ok(!JSON.stringify([preview, minted, f.sessionStore]).includes(key), "the key never leaves its frames");
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
  // A definite refusal drops the pending fields so the next Pair this profile click starts fresh.
  const g = await codeFixture({local:{}});
  g.setBroker(msg => msg.op === "pair" ? {ok:false, error:"EnrollmentConflict"} : g.fakeBroker(msg));
  assert.equal((await g.popup({type:"pair"})).error, "EnrollmentConflict");
  assert.deepEqual([g.localStore.pairing, g.localStore.pairing_pending], [undefined, undefined]);
  // A workflow enrollment is never paired over.
  const h = await codeFixture();
  assert.match((await h.popup({type:"pair"})).error, /workflow enrollment/);
  assert.ok(!h.brokerCalls.some(m => m.op === "pair"));
});

test("unpair removes the permission and the pairing record", async () => {
  const {profile:paired, key} = PAIRED.pairing;
  const f = await codeFixture({local:PAIRED});
  await mintTab(f, 7);
  const hellos = f.sent.filter(m => m.type === "hello").length;
  assert.deepEqual(await f.popup({type:"unpair"}), {ok:true, profile:paired});
  assert.deepEqual(f.brokerCalls.at(-1), {op:"unpair", profile:paired, email, key});
  assert.deepEqual([f.perms.held, f.table(), f.localStore.pairing], [false, {}, undefined]);
  assert.equal(f.native.closed, true);
  assert.equal(f.sent.filter(m => m.type === "hello").length, hellos, "no credential, no new hello");
  // A retry after a lost reply treats Unauthorized as done; a first Unauthorized is a refusal.
  const g = await codeFixture({local:PAIRED});
  await mintTab(g, 7);
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
