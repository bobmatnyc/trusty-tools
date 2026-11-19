// The extension harness for the crate's own extension tree (#11211): the fake
// Chrome, native hosts and broker of python/trusty-browser/tests/test_worker.mjs,
// loading crates/trusty-browser/extension instead. Keep the two in step.
import assert from "node:assert/strict";
import {createHmac, webcrypto} from "node:crypto";
import {readFile} from "node:fs/promises";
import vm from "node:vm";

const source = await readFile(new URL("../../extension/worker.js",import.meta.url),"utf8");
const pagesSource = await readFile(new URL("../../extension/pages.js",import.meta.url),"utf8");
const codesSource = await readFile(new URL("../../extension/codes.js",import.meta.url),"utf8");
const readSource = await readFile(new URL("../../extension/pages_read.js",import.meta.url),"utf8");
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


function sign(role,h,serverNonce,key=secret) {
  const data = JSON.stringify(["trusty-browser-native-v1",role,"chrome-extension://"+id,
    h.profile,h.email,h.incarnation,h.nonce,serverNonce]);
  return createHmac("sha256",key).update(data).digest("hex");
}


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

const cdpSteps = f => f.cdp.map(c => c.method + (c.params?.type ? ":" + c.params.type : ""));

export {READ, cdpSteps, codeFixture, connectGrant, plain, seedRow, settle};
