// Extension tests for pages_frames.js: auto-attached frame sessions, the all-frames snapshot, frame-scoped refs
// and their refusals (#11362). Run: node --test crates/trusty-browser/tests/js/*.test.mjs
import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

import {codeFixture, connectGrant, plain, settle} from "./fixture.mjs";

const AUTO = {autoAttach:true, waitForDebuggerOnStart:false, flatten:true};
const quadAt = (x, y, w, h) => [x, y, x + w, y, x + w, y + h, x, y + h];
const ax = (...nodes) => ({nodes});
const node = (nodeId, role, name, backend, extra = {}) =>
  ({nodeId, role:{value:role}, name:{value:name}, backendDOMNodeId:backend, ...extra});

// The Omni layout: app.test (top, session null) holds a srcdoc frame F1 and the out-of-process embed T2
// (session S2), which holds a nested out-of-process frame T3 (session S3). S2 also auto-attaches a worker.
function omni(over = {}) {
  const trees = {
    null:{frame:{id:"F0", loaderId:"L0", url:"https://app.test/insights", securityOrigin:"https://app.test"},
      childFrames:[{frame:{id:"F1", loaderId:"L1", url:"about:srcdoc", securityOrigin:"https://app.test"}},
        {frame:{id:"T2", loaderId:"", url:"https://embed.test/omni", securityOrigin:"https://embed.test"}}]},
    S2:{frame:{id:"T2", loaderId:"L2", url:"https://embed.test/omni", securityOrigin:"https://embed.test"},
      childFrames:[{frame:{id:"T3", loaderId:"", url:"https://deep.test/x", securityOrigin:"https://deep.test"}}]},
    S3:{frame:{id:"T3", loaderId:"L3", url:"https://deep.test/x", securityOrigin:"https://deep.test"}},
  };
  const owners = {"null:F1":[11, quadAt(0, 0, 50, 50)], "null:T2":[12, quadAt(100, 200, 400, 300)],
    "S2:T3":[21, quadAt(10, 20, 200, 100)]};
  const trees_ax = {
    F0:ax(node(1, "RootWebArea", "Insights", 1, {childIds:[2, 3]}), node(2, "generic", "", 2, {parentId:1, childIds:[4]}),
      node(4, "button", "Export", 4, {parentId:2}), node(3, "Iframe", "", 12, {parentId:1})),
    F1:ax(node(1, "RootWebArea", "", 30, {childIds:[2]}), node(2, "StaticText", "Revenue  up", 31, {parentId:1})),
    T2:ax(node(1, "RootWebArea", "Omni", 40, {childIds:[2, 3]}), node(2, "textbox", "Ask", 41, {parentId:1,
      value:{value:"why"}}), node(3, "button", "hidden", 43, {parentId:1, ignored:true})),
    T3:ax(node(1, "canvas", "chart", 42)),
  };
  const model = {trees, owners, ax:trees_ax, quads:{"S3:42":quadAt(5, 6, 10, 10), "S2:41":quadAt(30, 40, 100, 20)},
    hits:{}, worlds:{S2:77, S3:78}, call:null, ...over};
  return async (method, params, session, emit) => {
    const key = String(session);
    if (method === "Target.setAutoAttach") {
      if (session === null) emit({tabId:7}, "Target.attachedToTarget", {sessionId:"S2",
        targetInfo:{targetId:"T2", type:"iframe", url:"https://embed.test/omni", parentFrameId:"F0"}});
      if (session === "S2") {
        emit({tabId:7, sessionId:"S2"}, "Target.attachedToTarget", {sessionId:"S3",
          targetInfo:{targetId:"T3", type:"iframe", url:"https://deep.test/x", parentFrameId:"T2"}});
        emit({tabId:7, sessionId:"S2"}, "Target.attachedToTarget", {sessionId:"W1",
          targetInfo:{targetId:"W", type:"worker", url:"https://embed.test/w.js"}});
        for (const extra of model.attach ?? []) emit({tabId:7, sessionId:"S2"}, "Target.attachedToTarget", extra);
      }
      return {};
    }
    if (method === "Page.getFrameTree") return {frameTree:model.trees[key]};
    if (method === "DOM.getFrameOwner") return {backendNodeId:model.owners[key + ":" + params.frameId][0]};
    if (method === "DOM.getBoxModel") {
      const hit = Object.entries(model.owners).find(([k, [b]]) => k.startsWith(key + ":") && b === params.backendNodeId);
      return {model:{content:hit[1][1]}};
    }
    if (method === "Accessibility.getFullAXTree") {
      if (model.unreadable === params.frameId) throw new Error("Frame not found");
      return model.ax[params.frameId];
    }
    if (method === "DOM.getContentQuads") {
      const q = model.quads[key + ":" + params.backendNodeId];
      if (!q) throw new Error("No node with given id found");
      return {quads:[q]};
    }
    if (method === "DOM.getNodeForLocation") {
      if (model.hits[key] !== undefined) return {backendNodeId:model.hits[key]};
      // A point over a frame owner hits the owner element, as Chrome's hit test does.
      const owner = Object.entries(model.owners).find(([k, [, q]]) => k.startsWith(key + ":") &&
        params.x >= q[0] && params.x <= q[4] && params.y >= q[1] && params.y <= q[5]);
      if (owner) return {backendNodeId:owner[1][0]};
      return {backendNodeId:Number(Object.keys(model.quads).find(k => k.startsWith(key + ":"))?.split(":")[1])};
    }
    if (method === "Page.createIsolatedWorld") {
      if (!model.worlds[key]) throw new Error("Cannot create isolated world");
      return {executionContextId:model.worlds[key]};
    }
    if (method === "DOM.resolveNode") return {object:{objectId:"obj-" + params.backendNodeId}};
    if (method === "Runtime.callFunctionOn") { model.call = {session, params}; return {result:{value:model.value ?? {ok:true}}}; }
    return undefined;
  };
}

const sends = (f, method) => plain(f.cdp.filter(c => c.method === method));

test("auto-attach reaches nested out-of-process frames and detaches other targets", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni();
  const r = await f.command("page_snapshot", {grant, tab:7, document:"doc-7"});
  assert.equal(r.ok, true, JSON.stringify(r));
  assert.deepEqual(sends(f, "Target.setAutoAttach").map(c => [c.session ?? null, c.params]),
    [[null, AUTO], ["S2", AUTO], ["S3", AUTO]]);
  assert.ok(f.cdp.some(c => c.method === "Target.detachFromTarget" && c.session === "S2" && c.params.sessionId === "W1"),
    "the worker target is detached at once");
  assert.equal(f.cdp.filter(c => c.method === "attach").length, 1, "one attach per tab");
});

test("every child session is detached before the tab, and nothing is sent after close", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni();
  await f.command("page_snapshot", {grant, tab:7, document:"doc-7"});
  const tail = f.cdp.slice(-3).map(c => [c.method, c.session ?? null, c.params?.sessionId ?? null]);
  assert.deepEqual(tail, [["Target.detachFromTarget", "S2", "S3"], ["Target.detachFromTarget", null, "S2"],
    ["detach", null, null]]);
  const before = f.cdp.length;
  f.chrome.debugger.onEvent.fire({tabId:7}, "Target.attachedToTarget", {sessionId:"S9",
    targetInfo:{targetId:"T9", type:"iframe", url:"https://late.test/"}});
  await settle();
  assert.equal(f.cdp.length, before, "a late attach after detach sends nothing");
  assert.equal(f.chrome.debugger.onEvent.fns.size, 0, "no listener outlives the operation");
});

test("a snapshot reads every frame's accessibility tree within the node budget", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni();
  const r = await f.command("page_snapshot", {grant, tab:7, document:"doc-7"});
  const frames = r.result.frames;
  assert.deepEqual(frames.map(x => [x.frame, x.parent, x.loader, x.origin, x.root, x.owner, x.refused]), [
    ["F0", null, "L0", "https://app.test", false, null, null],
    ["F1", "F0", "L1", "https://app.test", false, [0, 0, 50, 50], null],
    ["T2", "F0", "L2", "https://embed.test", true, [100, 200, 400, 300], null],
    ["T3", "T2", "L3", "https://deep.test", true, [10, 20, 200, 100], null],
  ]);
  assert.deepEqual(frames[0].nodes, [[0, "RootWebArea", "Insights", 1, null], [1, "button", "Export", 4, null],
    [1, "Iframe", "", 12, null]], "generic rises, its child keeps the parent's depth");
  assert.deepEqual(frames[1].nodes, [[0, "RootWebArea", "", 30, null], [1, "StaticText", "Revenue up", 31, null]]);
  assert.deepEqual(frames[2].nodes, [[0, "RootWebArea", "Omni", 40, null], [1, "textbox", "Ask", 41, "why"]],
    "an ignored node is dropped");
  assert.equal(r.result.truncated, false);
  // The shared budget: a compact past it stops and says so.
  const ctx = vm.createContext({});
  vm.runInContext(await readFile(new URL("../../extension/pages_frames.js", import.meta.url), "utf8"), ctx);
  const budget = {left:1, truncated:false};
  const out = ctx.TrustyFrames.compact([node(1, "button", "a", 5), node(2, "button", "b", 6)], budget);
  assert.equal(out.length, 1);
  assert.equal(budget.truncated, true);
});

test("the pdf viewer and an unreadable frame are listed refused with a reason", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const model = omni({unreadable:"F1", attach:[{sessionId:"P1", targetInfo:{targetId:"PV", type:"iframe",
    url:"chrome-extension://mhjfbmdgcfjbbpaeojofohoefgiehjai/index.html", parentFrameId:"T3"}}]});
  f.tabs.get(7).frames = model;
  const r = await f.command("page_snapshot", {grant, tab:7, document:"doc-7"});
  const by = Object.fromEntries(r.result.frames.map(x => [x.frame, x]));
  assert.equal(by.F1.refused, "FrameUnreachable");
  assert.deepEqual(by.F1.nodes, []);
  assert.equal(by.T3.refused, "PdfViewer");
  assert.ok(sends(f, "Target.detachFromTarget").some(c => c.params.sessionId === "P1"), "the viewer is never entered");
});

test("a snapshot reports the tabs the leased tab opened", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni();
  f.tabs.get(9).opener = 7;
  f.tabs.get(9).window = 3;
  const r = await f.command("page_snapshot", {grant, tab:7, document:"doc-7"});
  assert.deepEqual(r.result.popups, [{tab:9, window:3}]);
});

test("a snapshot caps the opened tabs it reports", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni();
  for (let id = 100; id < 160; id++) f.tabs.set(id, {url:"https://a.test/", opener:7, window:2});
  const r = await f.command("page_snapshot", {grant, tab:7, document:"doc-7"});
  assert.equal(r.result.popups.length, 50);
  assert.equal(r.result.truncated, true);
});

test("a snapshot whose attach fails on the same document answers DebuggerBusy", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).busy = true;
  const r = await f.command("page_snapshot", {grant, tab:7, document:"doc-7"});
  assert.deepEqual([r.ok, r.error, r.tab], [false, "DebuggerBusy", 7]);
  const moved = await f.command("page_snapshot", {grant, tab:7, document:"doc-old"});
  assert.equal(moved.error, "DocumentChanged");
});

test("a click inside a nested out-of-process frame lands at the summed offset", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni();
  const r = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:{frame:"T3", loader:"L3", node:42}});
  assert.equal(r.ok, true, JSON.stringify(r));
  const input = sends(f, "Input.dispatchMouseEvent");
  // Centre (10, 11) in T3, plus T3's owner (10, 20) in T2, plus T2's owner (100, 200) in the top frame.
  assert.deepEqual(input.map(c => [c.params.type, c.params.x, c.params.y, c.session ?? null]),
    [["mouseMoved", 120, 231, null], ["mousePressed", 120, 231, null], ["mouseReleased", 120, 231, null]]);
  assert.equal(f.cdp.at(-1).method, "detach");
  assert.equal(f.cdp.filter(c => c.method === "attach").length, 1);
});

test("a covered frame element answers Obscured", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni({hits:{S3:99}});
  f.chrome.debugger.sendCommand = (orig => async (d, m, p) => m === "Runtime.callFunctionOn" ? {result:{value:false}}
    : orig(d, m, p))(f.chrome.debugger.sendCommand);
  const r = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:{frame:"T3", loader:"L3", node:42}});
  assert.deepEqual([r.error, r.tab], ["Obscured", 7]);
  assert.equal(sends(f, "Input.dispatchMouseEvent").length, 0);
});

test("a navigated frame answers DocumentChanged and a detached one StaleElement", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni();
  const moved = await f.command("page_hover", {grant, tab:7, document:"doc-7", ref:{frame:"T2", loader:"L2-old", node:41}});
  assert.deepEqual([moved.error, moved.tab], ["DocumentChanged", 7]);
  const gone = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:{frame:"T8", loader:"L8", node:4}});
  assert.deepEqual([gone.error, gone.tab], ["StaleElement", 7]);
  const node = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:{frame:"T2", loader:"L2", node:404}});
  assert.equal(node.error, "StaleElement", "a node no longer in the frame");
  assert.equal(sends(f, "Input.dispatchMouseEvent").length, 0);
});

test("a frame ref runs the page function in an isolated world of its frame", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  let model;
  f.tabs.get(7).frames = (m => (...a) => { model = a; return m(...a); })(omni());
  const r = await f.command("page_type", {grant, tab:7, document:"doc-7", ref:{frame:"T2", loader:"L2", node:41},
    text:"revenue by week"});
  assert.equal(r.ok, true, JSON.stringify(r));
  assert.deepEqual(r.result, {document:"doc-7"});
  const world = sends(f, "Page.createIsolatedWorld")[0];
  assert.deepEqual([world.session, world.params.frameId, world.params.grantUniveralAccess], ["S2", "T2", false]);
  const resolve = sends(f, "DOM.resolveNode")[0];
  assert.deepEqual([resolve.session, resolve.params], ["S2", {backendNodeId:41, executionContextId:77}]);
  const call = sends(f, "Runtime.callFunctionOn")[0];
  assert.equal(call.session, "S2");
  assert.deepEqual(call.params.arguments[0].value, ["@el", "type", "revenue by week"]);
  assert.match(call.params.functionDeclaration, /function pageKeys/);
  assert.equal(f.ran.length, 0, "no executeScript reached the top document");
  assert.ok(model);
});

test("a frame whose isolated world is refused answers FrameScriptBlocked", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni({worlds:{}});
  const r = await f.command("page_select", {grant, tab:7, document:"doc-7", ref:{frame:"T2", loader:"L2", node:41},
    value:"a"});
  assert.deepEqual([r.error, r.tab], ["FrameScriptBlocked", 7]);
  assert.equal(f.cdp.at(-1).method, "detach");
});

test("a drag across two frames answers NotDraggable and a page refusal inside a frame is relayed", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni();
  const across = await f.command("page_drag", {grant, tab:7, document:"doc-7", ref:{frame:"T2", loader:"L2", node:41},
    target:{frame:"T3", loader:"L3", node:42}});
  assert.equal(across.error, "NotDraggable");
  f.tabs.get(7).frames = omni({value:{error:"NotEditable"}});
  const field = await f.command("page_type", {grant, tab:7, document:"doc-7", ref:{frame:"T2", loader:"L2", node:41},
    text:"x"});
  assert.deepEqual([field.error, field.tab], ["NotEditable", 7]);
});

test("a snapshot of 2500 CJK nodes and long URLs stays under the native frame and says it is truncated", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const long = n => `https://app.test/${n}/` + "q".repeat(2000);
  const cjk = "\u6f22".repeat(160);
  const many = Array.from({length:2500}, (_, i) => node(i + 2, "StaticText", cjk, 1000 + i, {parentId:1}));
  const children = Array.from({length:180}, (_, i) => ({frame:{id:`C${i}`, loaderId:`LC${i}`, url:long(i),
    securityOrigin:"https://app.test"}}));
  f.tabs.get(7).frames = omni({
    trees:{null:{frame:{id:"F0", loaderId:"L0", url:long("top"), securityOrigin:"https://app.test"}, childFrames:children}},
    ax:{F0:ax(node(1, "RootWebArea", "Insights", 1, {childIds:many.map(n => n.nodeId)}), ...many)}});
  const r = await f.command("page_snapshot", {grant, tab:7, document:"doc-7"});
  assert.equal(r.ok, true, JSON.stringify(r).slice(0, 300));
  const bytes = Buffer.byteLength(JSON.stringify(r.result), "utf8");
  assert.ok(bytes < 1024 * 1024, `the reply is ${bytes} bytes`);
  assert.equal(r.result.truncated, true);
  assert.ok(r.result.frames.length <= 150, `${r.result.frames.length} frames`);
  // A node chain deeper than the depth cap stops there and says so.
  const ctx = vm.createContext({});
  vm.runInContext(await readFile(new URL("../../extension/pages_frames.js", import.meta.url), "utf8"), ctx);
  const chain = Array.from({length:60}, (_, i) => node(i + 1, "group", `g${i}`, i + 1,
    {...(i ? {parentId:i} : {}), childIds:[i + 2]}));
  const budget = {left:2500, bytes:700000, truncated:false};
  const out = ctx.TrustyFrames.compact(chain, budget);
  assert.ok(out.every(([depth]) => depth <= 40), "no node is deeper than the cap");
  assert.equal(budget.truncated, true);
});

test("the covered-element check resolves both nodes in an isolated world, not the page's", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni({hits:{S3:99}});
  f.chrome.debugger.sendCommand = (orig => async (d, m, p) => m === "Runtime.callFunctionOn" &&
    /contains/.test(p.functionDeclaration) ? {result:{value:true}} : orig(d, m, p))(f.chrome.debugger.sendCommand);
  const r = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:{frame:"T3", loader:"L3", node:42}});
  assert.equal(r.ok, true, JSON.stringify(r));
  const resolved = sends(f, "DOM.resolveNode").filter(c => c.session === "S3").map(c => c.params);
  assert.deepEqual(resolved, [{backendNodeId:42, executionContextId:78}, {backendNodeId:99, executionContextId:78}]);
  const world = sends(f, "Page.createIsolatedWorld").filter(c => c.session === "S3")[0];
  assert.equal(world?.params.frameId, "T3");
});

test("an overlay over the frame in its parent document answers Obscured", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  f.tabs.get(7).frames = omni({hits:{null:55}});
  const r = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:{frame:"T3", loader:"L3", node:42}});
  assert.deepEqual([r.error, r.tab], ["Obscured", 7]);
  const probes = sends(f, "DOM.getNodeForLocation").map(c => [c.session ?? null, c.params.x, c.params.y]);
  assert.deepEqual(probes.slice(0, 3), [["S3", 10, 11], ["S2", 20, 31], [null, 120, 231]], "each level is hit-tested");
  assert.equal(sends(f, "Input.dispatchMouseEvent").length, 0);
});

test("a frame that navigates between the move and the press answers DocumentChanged", async () => {
  const f = await codeFixture();
  const {grant} = await connectGrant(f);
  const base = omni();
  let moved = false;
  f.tabs.get(7).frames = async (method, params, session, emit) => {
    if (method === "Input.dispatchMouseEvent" && params.type === "mouseMoved") moved = true;
    const out = await base(method, params, session, emit);
    if (moved && method === "Page.getFrameTree" && session === "S3")
      return {frameTree:{frame:{...out.frameTree.frame, loaderId:"L3-new"}}};
    return out;
  };
  const r = await f.command("page_click", {grant, tab:7, document:"doc-7", ref:{frame:"T3", loader:"L3", node:42}});
  assert.deepEqual([r.error, r.tab], ["DocumentChanged", 7]);
  assert.deepEqual(sends(f, "Input.dispatchMouseEvent").map(c => c.params.type), ["mouseMoved"]);
});
