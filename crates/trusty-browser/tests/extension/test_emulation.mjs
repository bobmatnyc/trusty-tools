// Leased-tab emulation (#11371): JS off, prefers-color-scheme and viewport over chrome.debugger, against the
// shipped extension with mocked chrome APIs. Run: node --test crates/trusty-browser/tests/extension/
import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const dir = new URL("../../extension/", import.meta.url);
const sources = await Promise.all(["pages_read.js", "pages.js"].map(f => readFile(new URL(f, dir), "utf8")));
const email = "qa@example.org";

// `fail` names CDP methods sendCommand rejects; `busy` makes attach reject.
function browser({fail = [], busy = false} = {}) {
  const log = [], attached = new Set(), stored = {};
  let onDetach = null;
  const chrome = {
    tabs:{get:async id => ({id, status:"complete"})},
    storage:{session:{get:async key => ({[key]:stored[key]}), set:async v => Object.assign(stored, v)}},
    debugger:{
      async attach({tabId}) {
        log.push(["attach", tabId]);
        if (busy || attached.has(tabId)) throw new Error("Another debugger is already attached");
        attached.add(tabId);
      },
      async detach({tabId}) {
        log.push(["detach", tabId]);
        if (!attached.delete(tabId)) throw new Error("Debugger is not attached");
      },
      async sendCommand({tabId}, method, params) {
        log.push([method, tabId, params]);
        if (!attached.has(tabId)) throw new Error("Debugger is not attached");
        if (fail.includes(method)) throw new Error("rejected");
        return {};
      },
      onDetach:{addListener(f) { onDetach = f; }},
    },
  };
  const ctx = vm.createContext({chrome, TextEncoder, URL, setTimeout, clearTimeout, crypto:globalThis.crypto});
  for (const source of sources) vm.runInContext(source, ctx);
  const cmds = ctx.TrustyPages.commands({identity:async () => email, grant:async g => g === "g" ? {scope:"profile"} : null,
    ended:async () => {}, remember:async () => {}, originOf:async () => "https://a.test"});
  return {cmds, log, attached, stored, detachedByUser:tabId => { attached.delete(tabId); onDetach?.({tabId}, "canceled_by_user"); }};
}
const ask = (b, op, fields, grant = "g") => b.cmds.command({id:1, op, email, grant, tab:1, ...fields});
const plain = v => JSON.parse(JSON.stringify(v));
const methods = b => b.log.map(([m]) => m);

test("javascript turns off and back on, and the last default detaches", async () => {
  const b = browser();
  const off = plain(await ask(b, "page_javascript", {enabled:false}));
  assert.deepEqual(off.result, {javascript:false, color_scheme:"none", width:0, height:0});
  assert.deepEqual(plain(b.log[1]), ["Emulation.setScriptExecutionDisabled", 1, {value:true}]);
  assert.ok(b.attached.has(1), "the tab stays attached while JS is off");
  assert.deepEqual(plain(b.stored.emulated), [1]);
  const on = plain(await ask(b, "page_javascript", {enabled:true}));
  assert.deepEqual(on.result, {javascript:true, color_scheme:"none", width:0, height:0});
  assert.ok(!b.attached.has(1), "all defaults again: detached, which clears every override");
  assert.deepEqual(plain(b.stored.emulated), []);
});

test("color scheme emulates light and dark, and none resets", async () => {
  const b = browser();
  for (const scheme of ["dark", "light"]) {
    const r = plain(await ask(b, "page_color_scheme", {scheme}));
    assert.equal(r.result.color_scheme, scheme);
    assert.deepEqual(plain(b.log.at(-1)), ["Emulation.setEmulatedMedia", 1,
      {features:[{name:"prefers-color-scheme", value:scheme}]}]);
  }
  assert.deepEqual(methods(b).filter(m => m === "attach"), ["attach"], "one attachment holds the overrides");
  const none = plain(await ask(b, "page_color_scheme", {scheme:"none"}));
  assert.equal(none.result.color_scheme, "none");
  assert.ok(!b.attached.has(1));
  await assert.rejects(ask(b, "page_color_scheme", {scheme:"sepia"}), e => e.message === "InvalidRequest");
});

test("viewport sets, combines with other overrides, resets, and checks bounds", async () => {
  const b = browser();
  await ask(b, "page_color_scheme", {scheme:"dark"});
  const set = plain(await ask(b, "page_viewport", {width:1440, height:900}));
  assert.deepEqual(set.result, {javascript:true, color_scheme:"dark", width:1440, height:900});
  assert.deepEqual(plain(b.log.at(-1)), ["Emulation.setDeviceMetricsOverride", 1,
    {width:1440, height:900, deviceScaleFactor:0, mobile:false}]);
  const cleared = plain(await ask(b, "page_viewport", {width:0, height:0}));
  assert.deepEqual(cleared.result, {javascript:true, color_scheme:"dark", width:0, height:0});
  assert.equal(methods(b).at(-1), "Emulation.clearDeviceMetricsOverride");
  assert.ok(b.attached.has(1), "the color scheme still holds the attachment");
  for (const [width, height] of [[199, 900], [1440, 4321], [0, 900], [7681, 200]]) {
    await assert.rejects(ask(b, "page_viewport", {width, height}), e => e.message === "InvalidRequest");
  }
  await assert.rejects(ask(b, "page_viewport", {width:1.5, height:900}), e => e.message === "InvalidRequest");
});

test("a reset detaches even after the grant row is gone; a clean tab never attaches", async () => {
  const b = browser();
  const clean = plain(await ask(b, "page_javascript", {enabled:true}));
  assert.equal(clean.result.javascript, true);
  assert.deepEqual(b.log, [], "a default on a clean tab sends nothing");
  await ask(b, "page_javascript", {enabled:false});
  const reset = plain(await ask(b, "page_emulation_reset", {}, "ended-grant"));
  assert.deepEqual(reset, {id:1, ok:true, email, result:{javascript:true, color_scheme:"none", width:0, height:0}, tab:1});
  assert.ok(!b.attached.has(1));
});

test("a busy debugger or a rejected command leaves no override and answers DebuggerBusy", async () => {
  const busy = browser({busy:true});
  await assert.rejects(ask(busy, "page_javascript", {enabled:false}), e => e.message === "DebuggerBusy");
  const failing = browser({fail:["Emulation.setScriptExecutionDisabled"]});
  await assert.rejects(ask(failing, "page_javascript", {enabled:false}), e => e.message === "DebuggerBusy");
  assert.ok(!failing.attached.has(1), "a half-applied override is detached");
  assert.deepEqual(plain(failing.stored.emulated), []);
});

test("resetAll detaches every recorded tab, and a user detach forgets the tab", async () => {
  const b = browser();
  await ask(b, "page_viewport", {width:800, height:600});
  b.stored.emulated = [1, 5];
  await b.cmds.resetAll();
  assert.ok(!b.attached.has(1));
  assert.deepEqual(b.log.filter(([m]) => m === "detach").map(([, t]) => t).sort(), [1, 5]);
  assert.deepEqual(plain(b.stored.emulated), []);
  await ask(b, "page_color_scheme", {scheme:"dark"});
  b.detachedByUser(1);
  assert.deepEqual(plain(b.stored.emulated), []);
  const again = plain(await ask(b, "page_color_scheme", {scheme:"light"}));
  assert.equal(again.result.color_scheme, "light", "a fresh attachment after the user's detach");
});
