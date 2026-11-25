// Leased-tab emulation (#11371) against the shipped extension in crates/trusty-browser/extension with a
// mocked chrome.debugger. Each reset path has a test that fails when the path is removed.
// Run: node --test crates/trusty-browser/tests/extension/
import assert from "node:assert/strict";
import {webcrypto} from "node:crypto";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const dir = new URL("../../extension/", import.meta.url);
const names = ["pages_read.js", "emulation.js", "pages.js", "worker.js", "codes.js", "pages_frames.js"];
const [readJs, emulationJs, pagesJs, workerJs, codesJs, framesJs] =
  await Promise.all(names.map(f => readFile(new URL(f, dir), "utf8")));
const email = "qa@example.org";
const DARK = {javascript:false, color_scheme:"dark", width:1440, height:900};
const RESET = {javascript:true, color_scheme:null, width:0, height:0};
const settle = async () => { for (let i = 0; i < 30; i++) await new Promise(r => setImmediate(r)); };

function event() {
  const fns = [];
  return {addListener(fn){fns.push(fn);}, removeListener(fn){fns.splice(fns.indexOf(fn) >>> 0, 1);},
    fire(...args){for (const fn of [...fns]) fn(...args);}};
}

// A chrome.debugger that tracks attachments and the CDP commands each tab received.
function debuggerMock({attached = [], slow = false} = {}) {
  const on = new Set(), log = [];
  return {on, log, api:{
    onDetach:event(), onEvent:event(),
    async attach({tabId}) { if (on.has(tabId)) throw new Error("busy"); on.add(tabId); log.push(["attach", tabId]); },
    async detach({tabId}) { if (!on.delete(tabId)) throw new Error("not attached"); log.push(["detach", tabId]); },
    async sendCommand({tabId}, method, params) {
      if (!on.has(tabId)) throw new Error("not attached");
      log.push([method, tabId, params]);
      if (method === "Runtime.evaluate") return {};
      return {};
    },
    async getTargets() { if (slow) await settle(); return attached.map(tabId => ({tabId, attached:true})); },
  }};
}

function pages() {
  const dbg = debuggerMock();
  const chrome = {debugger:dbg.api, tabs:{get:async id => ({id})},
    scripting:{async executeScript({target, func}) {
      if (func.name === "pageProbe") return [{documentId:"D1", result:["https://a.test", "T", "https://a.test/"]}];
      if (func.name === "pageTarget") return [{documentId:"D1", result:{ok:true, x:5, y:5}}];
      throw new Error("unexpected " + func.name);
    }}};
  const ctx = vm.createContext({chrome, TextEncoder, URL, setTimeout, clearTimeout});
  for (const source of [readJs, emulationJs, framesJs, pagesJs]) vm.runInContext(source, ctx);
  const cmds = ctx.TrustyPages.commands({identity:async () => email, grant:async () => ({scope:"profile"}),
    ended:async () => {}, remember:async () => {}, originOf:async () => null});
  const ask = (op, fields, grant = "g") => cmds.command({id:1, op, email, grant, ...fields});
  return {ctx, dbg, ask};
}
const methods = (dbg, tab) => dbg.log.filter(e => e[1] === tab).map(e => e[0]);

test("emulation sets JavaScript, color scheme and viewport, and holds the attachment", async () => {
  const p = pages();
  const reply = await p.ask("page_emulate", {tab:1, ...DARK});
  assert.deepEqual(JSON.parse(JSON.stringify(reply.result)), DARK);
  assert.deepEqual(methods(p.dbg, 1), ["attach", "Emulation.setScriptExecutionDisabled",
    "Emulation.setEmulatedMedia", "Emulation.setDeviceMetricsOverride"]);
  const sent = Object.fromEntries(p.dbg.log.filter(e => e[1] === 1 && e[2]).map(e => [e[0], e[2]]));
  assert.equal(sent["Emulation.setScriptExecutionDisabled"].value, true);
  assert.equal(sent["Emulation.setEmulatedMedia"].features[0].value, "dark");
  assert.equal(sent["Emulation.setDeviceMetricsOverride"].width, 1440);
  assert.ok(p.dbg.on.has(1), "the attachment is held, so Chrome keeps the overrides");
  // Light scheme, JavaScript back on, viewport cleared: the same held attachment.
  await p.ask("page_emulate", {tab:1, javascript:true, color_scheme:"light", width:0, height:0});
  assert.equal(p.dbg.log.filter(e => e[0] === "attach").length, 1);
  assert.equal(p.dbg.log.at(-1)[0], "Emulation.clearDeviceMetricsOverride");
});

test("the defaults reset the tab and detach", async () => {
  const p = pages();
  await p.ask("page_emulate", {tab:1, ...DARK});
  const reply = await p.ask("page_emulate", {tab:1, ...RESET});
  assert.deepEqual(JSON.parse(JSON.stringify(reply.result)), RESET);
  assert.ok(!p.dbg.on.has(1));
  assert.equal(p.dbg.log.at(-1)[0], "detach");
  assert.ok(methods(p.dbg, 1).includes("Emulation.clearDeviceMetricsOverride"));
  // A reset of a tab that was never emulated attaches nothing.
  await p.ask("page_emulate", {tab:2, ...RESET});
  assert.deepEqual(methods(p.dbg, 2), []);
});

test("a malformed emulation frame is refused before any attach", async () => {
  const p = pages();
  for (const bad of [{...DARK, color_scheme:"dim"}, {...DARK, width:1440, height:0}, {...DARK, javascript:"no"},
    {...DARK, width:10001}]) {
    await assert.rejects(p.ask("page_emulate", {tab:1, ...bad}), e => e.message === "InvalidRequest");
  }
  assert.deepEqual(p.dbg.log, []);
});

test("a click on an emulated tab shares the held attachment and keeps it", async () => {
  const p = pages();
  await p.ask("page_emulate", {tab:1, ...DARK});
  const reply = await p.ask("page_click", {tab:1, document:"D1", ref:"1.0", dialog:"dismiss"});
  assert.equal(reply.ok, true);
  assert.ok(p.dbg.on.has(1), "act() left the emulation attachment in place");
  assert.equal(p.dbg.log.filter(e => e[0] === "attach").length, 1, "no second attach, so no DebuggerBusy");
  // Without emulation, act() attaches and detaches on its own.
  await p.ask("page_click", {tab:3, document:"D1", ref:"1.0", dialog:"dismiss"});
  assert.deepEqual([methods(p.dbg, 3)[0], methods(p.dbg, 3).at(-1)], ["attach", "detach"]);
});

test("grant_end resets the tabs that grant emulated", async () => {
  const p = pages();
  await p.ask("page_emulate", {tab:1, ...DARK});
  await p.ask("grant_end", {reason:"revoked", last:true});
  assert.ok(!p.dbg.on.has(1));
});

test("a command under another grant resets the emulated tab first", async () => {
  const p = pages();
  await p.ask("page_emulate", {tab:1, ...DARK});
  await p.ask("page_tab", {tab:1}, "other");
  assert.ok(!p.dbg.on.has(1));
  assert.ok(methods(p.dbg, 1).includes("Emulation.clearDeviceMetricsOverride"));
});

test("a debugger detached by Chrome forgets the tab", async () => {
  const p = pages();
  await p.ask("page_emulate", {tab:1, ...DARK});
  p.dbg.on.delete(1);
  p.dbg.api.onDetach.fire({tabId:1}, "canceled_by_user");
  assert.equal(p.ctx.TrustyEmulation.held(1), false);
});

// The service worker with a fake native port, as test_worker_pairing.mjs builds it.
async function worker({attached = [], slow = false} = {}) {
  const dbg = debuggerMock({attached, slow});
  for (const tabId of attached) dbg.on.add(tabId);
  const native = {onMessage:event(), onDisconnect:event(), postMessage(){}, disconnect(){}};
  const store = {pairing:{profile:"5f0c6b1e-2d4a-4c8e-9b7a-1e2f3a4b5c6d", email, key:"a".repeat(64)}};
  const session = {incarnation:"epoch-1"};
  const chrome = {debugger:dbg.api,
    runtime:{id:"jabfhcolknbbdajilgpficniedhcfahc", onConnect:event(), onMessage:event(), onStartup:event(),
      onInstalled:event(), getManifest:() => ({version:"0.2.0"}), reload(){},
      connectNative:() => { dbg.log.push(["connectNative"]); return native; }},
    storage:{local:{setAccessLevel:async () => {}, set:async () => {}, remove:async () => {},
      get:async keys => Object.fromEntries([keys].flat().map(k => [k, structuredClone(store[k])]))},
    session:{get:async key => ({[key]:session[key]}), set:async data => { Object.assign(session, data); }}},
    identity:{getProfileUserInfo:async () => ({email}), onSignInChanged:event()},
    scripting:{getRegisteredContentScripts:async () => [], unregisterContentScripts:async () => {}},
    alarms:{onAlarm:event(), create(){}}, tabs:{get:async id => ({id})}};
  const scripts = {"pages.js":pagesJs, "pages_read.js":readJs, "codes.js":codesJs, "emulation.js":emulationJs,
    "pages_frames.js":framesJs,
    "indicator.js":""};
  const ctx = vm.createContext({chrome, crypto:webcrypto, TextEncoder, URL,
    TrustyControlIndicator:{clear(){}, clearAll(){}, grants:async () => {}}, setTimeout:() => 0, clearTimeout(){}});
  ctx.importScripts = (...files) => { for (const f of files) vm.runInContext(scripts[f], ctx); };
  vm.runInContext(workerJs, ctx);
  await settle();
  return {ctx, dbg, native};
}

test("a dropped native port resets every emulated tab", async () => {
  const w = await worker();
  await w.ctx.TrustyEmulation.apply({tab:4, grant:"g", ...DARK});
  assert.ok(w.dbg.on.has(4));
  w.native.onDisconnect.fire();
  await settle();
  assert.ok(!w.dbg.on.has(4), "the broker link is gone, so is the emulation");
});

test("a worker start releases an attachment a previous run left", async () => {
  const w = await worker({attached:[9]});
  assert.ok(!w.dbg.on.has(9));
});

// A slow sweep still finishes before the native port opens, so it cannot detach an act() attachment.
test("the worker connects only after the start sweep", async () => {
  const w = await worker({attached:[9], slow:true});
  await settle();
  const order = w.dbg.log.map(e => e[0]);
  assert.ok(order.includes("connectNative"), String(order));
  assert.ok(order.indexOf("detach") < order.indexOf("connectNative"), String(order));
});
