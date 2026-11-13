import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import {readFile} from "node:fs/promises";

const source = await readFile(new URL("../src/trusty_browser/extension/indicator.js", import.meta.url), "utf8");
function profile() {
  const view = {badge:"",title:"", messages:[],refuse:false};
  const timers = new Set();
  const ctx = {
    setTimeout(fn) { timers.add(fn); return fn; },
    clearTimeout(fn) { timers.delete(fn); },
    chrome: {
      tabs:{async sendMessage(tab, msg, target) {
        view.messages.push({tab, action:msg.action, claim:msg.claim, doc:target.documentId});
        return {shown:!view.refuse && msg.action === "show"};
      }},
      action:{
        async setBadgeBackgroundColor() {}, async setBadgeTextColor() {},
        async setBadgeText({text}) { view.badge = text; },
        async setTitle({title}) { view.title = title; }
      }
    }
  };
  vm.runInNewContext(source,ctx);
  return {control:ctx.TrustyControlIndicator,view,timers};
}
const state = (claim,workflow) => ({claim,workflow,ttl_ms:30000});

test("badge is per profile and banner is pinned to the leased document", async () => {
  const a = profile(), b = profile();
  await a.control.update(7,"doc-a",state("a","travel"));
  assert.equal(a.view.badge,"ON");
  assert.match(a.view.title,/travel/);
  assert.deepEqual(a.view.messages,[{tab:7,action:"show",claim:"a",doc:"doc-a"}]);
  assert.equal(b.view.badge,"");
  await a.control.clear(7,"other");
  assert.equal(a.view.badge,"ON");
  await a.control.clear(7,"a");
  assert.equal(a.view.badge,"");
  assert.equal(a.view.messages.at(-1).action,"clear");
});

test("expiry, renewal, and disconnect preserve unrelated leases", async () => {
  const {control,view,timers} = profile();
  await control.update(7,"doc-a",state("a","travel"));
  const oldExpiry = [...timers][0];
  await control.update(7,"doc-a",state("a","travel"));
  assert.ok(!timers.has(oldExpiry));
  await control.update(8,"doc-b",state("b","work"));
  await [...timers][0]();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(view.badge,"ON");
  assert.equal(view.title,"Trusty Browser controls work");
  await control.clearAll();
  assert.equal(view.badge,"");
  assert.equal(timers.size,0);
});

test("an unavailable content script prevents reporting active control", async () => {
  const {control,view} = profile();
  view.refuse = true;
  await assert.rejects(control.update(2,"doc",state("x","travel")), /IndicatorUnavailable/);
  assert.equal(view.badge,"");
});
