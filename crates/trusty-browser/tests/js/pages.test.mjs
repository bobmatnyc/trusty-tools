// Extension tests for the crate's own pages.js: tabs, screenshots and coordinate input (#11208, #11211).
// Run: node --test crates/trusty-browser/tests/js/*.test.mjs
import assert from "node:assert/strict";
import test from "node:test";

import {cdpSteps, codeFixture, connectGrant, plain, seedRow, settle} from "./fixture.mjs";

const email = "qa@example.org";
const M = {css_width:800, css_height:600, device_pixel_ratio:2, scroll_x:0, scroll_y:40};
// Tab `tab` answers pageView with `metrics` and `region`; Page.captureScreenshot answers `f.png`.
const withView = (f, tab = 7, metrics = M, region = null) => {
  f.tabs.get(tab).result = {ok:true, region, metrics};
  f.chrome.tabs.getZoom = async () => 1;
  const send = f.chrome.debugger.sendCommand;
  f.chrome.debugger.sendCommand = async (d, method, params) => {
    const out = await send(d, method, params);
    return method === "Page.captureScreenshot" ? {data:f.png ?? "iVBORw0KGgo="} : out;
  };
};
const shots = f => f.cdp.filter(c => c.method === "Page.captureScreenshot");
// A fake tabs.create/remove pair: the new tab is 12 on `origin`.
const tabSeams = f => {
  const created = [], removed = [];
  f.chrome.tabs.create = async ({url, active}) => {
    created.push({url, active});
    f.tabs.set(12, {doc:"doc-12", origin:new URL(url).origin, title:"New"});
    return {id:12, windowId:1};
  };
  f.chrome.tabs.remove = async tab => {
    if (!f.tabs.has(tab)) throw new Error("No tab");
    f.tabs.delete(tab);
    removed.push(tab);
  };
  return {created, removed};
};

test("a viewport screenshot is taken over the debugger with the metrics read after the attach", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  withView(f);
  const r = await f.command("page_screenshot", {grant, tab:7, document:"doc-7", ref:null});
  assert.deepEqual(r.result, {document:"doc-7", data:"iVBORw0KGgo=", region:{x:0, y:0, width:800, height:600},
    metrics:{...M, zoom:1}});
  assert.deepEqual(cdpSteps(f), ["attach", "Page.captureScreenshot", "detach"]);
  assert.equal(f.cdp[1].params.clip, undefined);
});

test("an oversized png is retaken at half and quarter scale, then refused CaptureTooLarge", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  withView(f);
  f.png = "A".repeat(700004);
  const big = await f.command("page_screenshot", {grant, tab:7, document:"doc-7", ref:null});
  assert.equal(big.error, "CaptureTooLarge");
  assert.deepEqual(shots(f).map(c => c.params.clip?.scale), [undefined, 0.5, 0.25]);
  assert.equal(f.cdp.at(-1).method, "detach");
});

test("an element screenshot clips its region in page pixels and a moved document is refused", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  withView(f, 7, M, {x:10, y:20, width:100, height:50});
  const el = await f.command("page_screenshot", {grant, tab:7, document:"doc-7", ref:"1.0"});
  assert.deepEqual(el.result.region, {x:10, y:20, width:100, height:50});
  assert.deepEqual(plain(f.cdp[1].params.clip), {x:10, y:60, width:100, height:50, scale:1});
  const stale = await f.command("page_screenshot", {grant, tab:7, document:"doc-old", ref:null});
  assert.equal(stale.error, "DocumentChanged");
});

test("a screenshot whose attach fails on the same document answers DebuggerBusy", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  withView(f);
  f.tabs.get(7).busy = true;
  const r = await f.command("page_screenshot", {grant, tab:7, document:"doc-7", ref:null});
  assert.deepEqual(r, {id:r.id, ok:false, error:"DebuggerBusy", tab:7});
  assert.equal(shots(f).length, 0);
});

test("a timed-out screenshot sends no capture after it returns", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  withView(f);
  f.png = "A".repeat(700004);
  f.tabs.get(7).hang = "Page.captureScreenshot";
  const pending = f.native.onMessage.fire({id:"slow", op:"page_screenshot", email, grant, tab:7, document:"doc-7", ref:null});
  await settle();
  const [key, bound] = [...f.timers].findLast(([, t]) => t.ms === 6000);
  f.timers.delete(key);
  bound.fn();
  await pending;
  await settle();
  assert.equal(f.cdp.at(-1).method, "detach");
  f.tabs.get(7).release();
  await settle();
  assert.equal(shots(f).length, 1, "a retake ran after the capture returned");
});

test("coordinate input refuses a changed viewport or an outside point and types as trusted text", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  withView(f);
  const metrics = {...M, zoom:1};
  const ok = await f.command("page_click_at", {grant, tab:7, document:"doc-7", x:100.25, y:50.25, metrics, dialog:"dismiss"});
  assert.deepEqual(ok.result, {document:"doc-7"});
  assert.deepEqual(cdpSteps(f), ["attach", "Page.enable", "Input.dispatchMouseEvent:mouseMoved",
    "Input.dispatchMouseEvent:mousePressed", "Input.dispatchMouseEvent:mouseReleased", "Runtime.evaluate", "detach"]);
  assert.equal(f.cdp[2].params.x, 100.25);
  f.cdp.length = 0;
  const moved = await f.command("page_click_at", {grant, tab:7, document:"doc-7", x:1, y:1,
    metrics:{...metrics, scroll_y:41}, dialog:"dismiss"});
  assert.deepEqual(moved, {id:moved.id, ok:false, error:"StaleCapture", tab:7});
  assert.ok(!f.cdp.some(c => c.method === "Input.dispatchMouseEvent"));
  const out = await f.command("page_click_at", {grant, tab:7, document:"doc-7", x:800, y:1, metrics, dialog:"dismiss"});
  assert.equal(out.error, "OutOfViewport");
  f.cdp.length = 0;
  const typed = await f.command("page_type_at", {grant, tab:7, document:"doc-7", x:5, y:5, metrics, dialog:"dismiss", text:"hi"});
  assert.deepEqual(typed.result, {document:"doc-7"});
  assert.deepEqual(plain(f.cdp.find(c => c.method === "Input.insertText").params), {text:"hi"});
  const bad = await f.command("page_click_at", {grant, tab:7, document:"doc-7", x:5, y:5, metrics:{}, dialog:"dismiss"});
  assert.equal(bad.error, "InvalidRequest");
});

test("an opened tab is a background tab and a closed tab is gone", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const {created, removed} = tabSeams(f);
  const opened = await f.command("page_open_tab", {grant, url:"https://a.test/x"});
  assert.deepEqual(created, [{url:"https://a.test/x", active:false}]);
  assert.equal(opened.result.tab, 12);
  assert.equal(opened.result.window, 1);
  assert.deepEqual(Object.keys(opened.result), ["tab","window","access","document","origin","title"]);
  const badUrl = await f.command("page_open_tab", {grant, url:"ftp://a.test/"});
  assert.equal(badUrl.error, "InvalidUrl");
  const closed = await f.command("page_close_tab", {grant, tab:12});
  assert.deepEqual(closed.result, {closed:true});
  assert.deepEqual(removed, [12]);
});

test("a close for a tab the worker cannot find answers TabNotFound and removes nothing", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const {removed} = tabSeams(f);
  const gone = await f.command("page_close_tab", {grant, tab:12});
  assert.deepEqual(gone, {id:gone.id, ok:false, error:"TabNotFound", tab:12});
  assert.deepEqual(removed, []);
});

test("a tab grant may neither open nor close a tab", async () => {
  const f = await codeFixture();
  await connectGrant(f);
  const grant = seedRow(f, "g-000000000099", "tab", 7);
  const {created, removed} = tabSeams(f);
  const open = await f.command("page_open_tab", {grant, url:"https://a.test/x"});
  assert.equal(open.error, "OutOfScope");
  const close = await f.command("page_close_tab", {grant, tab:7});
  assert.deepEqual(close, {id:close.id, ok:false, error:"OutOfScope", tab:7});
  assert.deepEqual([created, removed], [[], []]);
});

test("a tab that fails after it is created is closed and answers OutcomeUnknown", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const {created, removed} = tabSeams(f);
  f.chrome.tabs.onUpdated.addListener = () => { throw new Error("listener"); };
  const r = await f.command("page_open_tab", {grant, url:"https://a.test/x"});
  assert.deepEqual(r, {id:r.id, ok:false, error:"OutcomeUnknown"});
  assert.equal(created.length, 1);
  assert.deepEqual(removed, [12]);
});
