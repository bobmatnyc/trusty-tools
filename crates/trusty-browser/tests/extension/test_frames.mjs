// Cross-origin child-frame reads in page_read / page_find (#11301) and actions on frame-scoped refs (#11808),
// against the shipped extension in crates/trusty-browser/extension with mocked chrome APIs.
// Run: node --test crates/trusty-browser/tests/extension/
import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const dir = new URL("../../extension/", import.meta.url);
const sources = await Promise.all(["pages_read.js", "emulation.js", "pages_frames.js", "pages.js"].map(f => readFile(new URL(f, dir), "utf8")));
const email = "qa@example.org";
const READ = {scope_ref:null, max_chars:50000, viewport_only:false, include_text:false};
const FIND = {role:null, name:"CHILD-LABEL", text:null, within_ref:null, limit:10};
// #11284: a denied child's text, which no read result may carry.
const SECRET = "CHILD-SECRET-TEXT", TEXT_READ = {...READ, include_text:true};
const leaks = r => JSON.stringify(r).includes(SECRET);
const TOP = "https://a.test", CHILD = "https://b.test", GRAND = "https://c.test";

const element = (ref, name) => ({ref, tag:"button", role:"button", name});
const read = (origin, elements, text = "") => ({origin, url:origin + "/", title:"T", text, elements, truncated:false});
const found = (origin, matches) => ({origin, matches:matches.map(m => ({...m, in:{role:"form", name:"", ref:"1.9"}})),
  truncated:false});

// A tab whose frames are `frames` ({id, origin, document, root, children, elements, text}); frame 0 is the top.
// `allowed` holds the granted host patterns; `gone` the documentIds whose frame navigated away; `input` the
// CDP commands a trusted click sent. #11808: an injected action types into `f.value`, and a trailing newline
// submits it, which sets the frame's text to "Echo: <value>". `cdp` shapes the top session's DOM: `owners`
// are the <iframe> nodes ({node, frame, url}), `quad` the owner's content quad, `at` the hit-tested node, and
// `hit` whether the child's witness saw the trusted click.
function browser({frames, allowed = [TOP + "/*"], injected = null, cdp = {}}) {
  const calls = [], gone = new Set(), input = [];
  const view = {owners:[{node:50, frame:"F7", url:CHILD + "/"}], quad:[100, 200, 500, 200, 500, 500, 100, 500],
    at:{backendNodeId:50}, hit:true, ...cdp};
  const DOMS = {
    "DOM.getDocument":() => ({root:{nodeName:"#document", children:view.owners.map(o =>
      ({nodeName:"IFRAME", backendNodeId:o.node, frameId:o.frame}))}}),
    "Page.getFrameTree":() => ({frameTree:{frame:{id:"TOP", url:TOP + "/"}}}),
    "DOM.getBoxModel":() => ({model:{content:view.quad}}),
    "DOM.getNodeForLocation":() => view.at,
  };
  const byDoc = new Map(frames.map(f => [f.document, f]));
  const contains = ({origins:[pattern]}) => allowed.includes(pattern) || allowed.includes("https://*/*");
  const reachable = f => (injected ?? (f => contains({origins:[new URL(f.origin).protocol + "//" +
    new URL(f.origin).hostname + "/*"]})))(f);
  const answer = (f, func, args) => {
    if (func.name === "pageProbe") return [f.origin, "T", f.origin + "/"];
    if (func.name === "pageFrames") return {origin:f.origin, url:f.origin + "/", root:f.root ?? f.id !== 0,
      children:f.children ?? []};
    if (func.name === "pageWitnessed") return {ok:true, hit:view.hit};
    const ACTS = {pageKeys:0, pageSelect:0, pageHover:0, pageTarget:0, pageScroll:1, pageDrag:0, pageSetFiles:0,
      pageWitness:0};
    if (func.name in ACTS) {
      if (!f.elements.some(e => e.ref === args[ACTS[func.name]])) return {error:"StaleElement"};
      if (func.name === "pageTarget") return {ok:true, x:10, y:20, vw:400, vh:300, url:f.origin + "/#top"};
      if (func.name === "pageKeys" && args[1] === "type") {
        f.value = args[2].replace(/\n$/, "");
        if (args[2].endsWith("\n")) f.text = "Echo: " + f.value;
      }
      if (func.name === "pageSelect") return {ok:true, changed:true, value:args[1]};
      if (func.name === "pageScroll") return {ok:true, view:{scroll_y:0, scroll_height:10, viewport_height:10,
        grew:false, at_end:true}};
      return {ok:true};
    }
    const [mode, , options] = args;
    const scope = options.scope_ref ?? options.within_ref;
    if (scope != null && !f.elements.some(e => e.ref === scope)) return {error:"StaleElement"};
    const list = scope != null ? f.elements.filter(e => e.ref !== scope) : f.elements;
    return mode === "read" ? read(f.origin, list, options.include_text ? f.text ?? "" : "") : found(f.origin, list.filter(e => e.name.includes(options.name)));
  };
  const chrome = {
    tabs:{get:async id => ({id, status:"complete"}), onUpdated:{addListener(){}, removeListener(){}}},
    debugger:{attach:async () => {}, detach:async () => {}, onEvent:{addListener(){}, removeListener(){}},
      getTargets:async () => view.owners.map(o => ({id:o.frame, type:"other", url:o.url})),
      sendCommand:async (_, method, params) => { input.push([method, params]); return DOMS[method]?.() ?? {}; }},
    permissions:{contains:async query => contains(query)},
    scripting:{async executeScript({target, func, args = []}) {
      calls.push({target, func:func.name, args});
      if (target.allFrames) {
        return frames.filter(f => f.id === 0 || reachable(f))
          .map(f => ({frameId:f.id, documentId:f.document, result:answer(f, func, args)}));
      }
      const f = target.frameIds ? frames.find(x => x.id === 0) : byDoc.get(target.documentIds[0]);
      if (!f || gone.has(f.document) || !reachable(f)) throw new Error("No frame with id");
      // `later` models a document that answers from another origin after discovery.
      const now = target.documentIds && f.later ? {...f, origin:f.later} : f;
      return [{frameId:f.id, documentId:f.document, result:answer(now, func, args)}];
    }},
  };
  const ctx = vm.createContext({chrome, TextEncoder, URL, setTimeout, clearTimeout, crypto:globalThis.crypto});
  for (const source of sources) vm.runInContext(source, ctx);
  return {ctx, chrome, calls, gone, input};
}

function commands(b, row = {scope:"profile"}) {
  return b.ctx.TrustyPages.commands({identity:async () => email, grant:async () => row, ended:async () => {},
    remember:async () => {}, originOf:async () => TOP});
}
const ask = (cmds, op, fields, tab = 1) => cmds.command({id:1, op, email, grant:"g", tab, document:null, ...fields});
const failed = async (promise, error) => {
  await assert.rejects(promise, e => e.message === error);
};
const plain = value => JSON.parse(JSON.stringify(value));

const page = (child = {}) => [
  {id:0, origin:TOP, document:"D0", children:[CHILD], elements:[element("11.0", "Top button")]},
  {id:7, origin:CHILD, document:"D7", elements:[element("22.0", "CHILD-LABEL-7")], ...child},
];

test("a granted child frame is read with attribution", async () => {
  const b = browser({frames:page(), allowed:[TOP + "/*", CHILD + "/*"]});
  const cmds = commands(b);
  const reply = plain(await ask(cmds, "page_read", READ));
  const r = reply.result;
  assert.equal(r.document, "D0");
  assert.equal(r.origin, TOP);
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"},
    {id:7, origin:CHILD, url:CHILD + "/", document:"D7"}]);
  assert.deepEqual(r.denied_frames, []);
  assert.deepEqual(r.elements.map(e => [e.ref, e.frame, e.name]),
    [["11.0", 0, "Top button"], ["7:22.0", 7, "CHILD-LABEL-7"]]);
  const find = plain(await ask(cmds, "page_find", FIND)).result;
  assert.deepEqual(find.matches.map(m => [m.ref, m.frame, m.name, m.in.ref]), [["7:22.0", 7, "CHILD-LABEL-7", "7:1.9"]]);
  assert.equal(find.frames[1].origin, CHILD);
  // A profile-wide grant covers every child origin.
  const wide = browser({frames:page(), allowed:["https://*/*"]});
  assert.equal(plain(await ask(commands(wide), "page_read", READ)).result.elements[1].name, "CHILD-LABEL-7");
});

test("a child frame without host permission is denied with no content", async () => {
  // Chrome does not inject into the child; only the top's declared src names it.
  const b = browser({frames:page({text:SECRET})});
  const r = plain(await ask(commands(b), "page_read", TEXT_READ)).result;
  assert.ok(!leaks(r));
  assert.deepEqual(r.denied_frames, [{origin:CHILD, reason:"no_permission"}]);
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"}]);
  assert.ok(!JSON.stringify(r).includes("CHILD-LABEL"));
  assert.ok(!b.calls.some(c => c.target.documentIds), "no child document was read");
  // activeTab-style access: Chrome injected the child, yet the extension holds no permission for its origin.
  const loose = browser({frames:page({text:SECRET}), injected:() => true});
  assert.ok(!leaks(plain(await ask(commands(loose), "page_read", TEXT_READ))));
  const f = plain(await ask(commands(loose), "page_find", FIND)).result;
  assert.deepEqual(f.matches, []);
  assert.deepEqual(f.denied_frames, [{origin:CHILD, reason:"no_permission"}]);
  assert.ok(!loose.calls.some(c => c.target.documentIds));
});

test("grants are not inherited by grandchildren", async () => {
  const frames = [...page({children:[GRAND]}),
    {id:9, origin:GRAND, document:"D9", elements:[element("33.0", "CHILD-LABEL-GRAND")]}];
  const b = browser({frames, allowed:[TOP + "/*", CHILD + "/*"], injected:() => true});
  const r = plain(await ask(commands(b), "page_read", READ)).result;
  assert.deepEqual(r.elements.map(e => e.name), ["Top button", "CHILD-LABEL-7"]);
  assert.deepEqual(r.denied_frames, [{origin:GRAND, reason:"no_permission"}]);
});

test("a tab outside the grant's scope reads no frame", async () => {
  const b = browser({frames:page(), allowed:["https://*/*"]});
  await failed(ask(commands(b, {scope:"tab", tab:1}), "page_read", READ, 2), "OutOfScope");
  await failed(ask(commands(b, {scope:"tab", tab:1}), "page_find", FIND, 2), "OutOfScope");
  assert.equal(b.calls.length, 0);
});

test("a child ref is refused once its frame navigated", async () => {
  const b = browser({frames:page({elements:[element("22.0", "CHILD-LABEL-7"), element("22.1", "Inner")]}),
    allowed:[TOP + "/*", CHILD + "/*"]});
  const cmds = commands(b);
  await failed(ask(cmds, "page_read", {...READ, scope_ref:"7:22.0"}), "StaleElement");
  await ask(cmds, "page_read", READ);
  const scoped = plain(await ask(cmds, "page_read", {...READ, scope_ref:"7:22.0"})).result;
  assert.deepEqual(scoped.elements.map(e => e.ref), ["7:22.1"]);
  assert.equal(scoped.document, "D0");
  assert.equal(scoped.frames[1].document, "D7");
  const within = plain(await ask(cmds, "page_find", {...FIND, name:"Inner", within_ref:"7:22.0"})).result;
  assert.deepEqual(within.matches.map(m => m.ref), ["7:22.1"]);
  b.gone.add("D7");
  await failed(ask(cmds, "page_read", {...READ, scope_ref:"7:22.0"}), "StaleElement");
  await failed(ask(cmds, "page_find", {...FIND, within_ref:"7:22.0"}), "StaleElement");
  // #11808: drag and upload still take no frame ref; the six actions do (see the tests below).
  await failed(ask(cmds, "page_drag", {ref:"7:22.0", target:"7:22.1"}), "InvalidRequest");
  await failed(ask(cmds, "page_upload", {ref:"7:22.0", files:[{name:"a", type:"", data:""}]}), "InvalidRequest");
});

test("the frame count and byte share stay inside the result budget", async () => {
  const many = Array.from({length:12}, (_, i) => ({id:i + 1, origin:`https://f${i}.test`, document:"F" + i,
    elements:Array.from({length:40}, (_, n) => element(`5.${n}`, "x".repeat(150)))}));
  const frames = [{id:0, origin:TOP, document:"D0", elements:[element("11.0", "Top button")]}, ...many];
  const b = browser({frames, allowed:["https://*/*"]});
  const r = plain(await ask(commands(b), "page_read", {...READ, max_chars:20000})).result;
  assert.equal(r.frames.length, 9);
  assert.deepEqual(r.denied_frames.map(d => d.reason), ["frame_limit", "frame_limit", "frame_limit", "frame_limit"]);
  assert.ok(new TextEncoder().encode(JSON.stringify(r)).length <= 20000);
  assert.equal(r.truncated, true);
  const shares = b.calls.filter(c => c.target.documentIds && c.func === "pageInspect").map(c => c.args[2].max_chars);
  assert.deepEqual(new Set(shares), new Set([1250]));
});

test("a page without cross-origin frames reads exactly as before", async () => {
  const b = browser({frames:[{id:0, origin:TOP, document:"D0", elements:[element("11.0", "Top button")]}]});
  const r = plain(await ask(commands(b), "page_read", READ)).result;
  assert.deepEqual(Object.keys(r), ["document", "origin", "url", "title", "text", "elements", "truncated"]);
  assert.deepEqual(r.elements, [element("11.0", "Top button")]);
});

test("a root frame with an opaque origin is denied even under a profile-wide grant", async () => {
  const b = browser({frames:page({origin:"null", text:SECRET}), allowed:["https://*/*"], injected:() => true});
  const r = plain(await ask(commands(b), "page_read", TEXT_READ)).result;
  assert.ok(!leaks(r));
  assert.deepEqual(r.denied_frames, [{origin:"null", reason:"opaque"}]);
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"}]);
  assert.ok(!JSON.stringify(r).includes("CHILD-LABEL"));
  assert.ok(!b.calls.some(c => c.target.documentIds), "no child document was injected");
});

test("a child that navigated between discovery and the pinned read is denied as navigated", async () => {
  const allowed = [TOP + "/*", CHILD + "/*"];
  // The document is gone by the time the pinned read runs.
  const gone = browser({frames:page({text:SECRET}), allowed});
  gone.gone.add("D7");
  const r = plain(await ask(commands(gone), "page_read", TEXT_READ)).result;
  assert.ok(!leaks(r));
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"}]);
  assert.deepEqual(r.denied_frames, [{origin:CHILD, reason:"navigated"}]);
  assert.ok(!JSON.stringify(r).includes("CHILD-LABEL"));
  assert.ok(gone.calls.some(c => c.target.documentIds), "the pinned read was attempted");
  // The same document answers from another origin.
  const moved = browser({frames:page({later:GRAND, text:SECRET}), allowed:[...allowed, GRAND + "/*"]});
  const m = plain(await ask(commands(moved), "page_read", TEXT_READ)).result;
  assert.ok(!leaks(m));
  assert.deepEqual(m.denied_frames, [{origin:CHILD, reason:"navigated"}]);
  const f = plain(await ask(commands(moved), "page_find", FIND)).result;
  assert.deepEqual(f.matches, []);
  assert.deepEqual(f.denied_frames, [{origin:CHILD, reason:"navigated"}]);
  assert.ok(!JSON.stringify(f).includes("CHILD-LABEL"));
});

test("a failing permission check denies the child with no content", async () => {
  const b = browser({frames:page({text:SECRET}), allowed:["https://*/*"], injected:() => true});
  b.chrome.permissions.contains = async () => { throw new Error("permissions unavailable"); };
  const r = plain(await ask(commands(b), "page_read", TEXT_READ)).result;
  assert.ok(!leaks(r));
  assert.deepEqual(r.denied_frames, [{origin:CHILD, reason:"no_permission"}]);
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"}]);
  assert.ok(!JSON.stringify(r).includes("CHILD-LABEL"));
  assert.ok(!b.calls.some(c => c.target.documentIds));
});

// #11284: runs the shipped pageInspect over a fake top document of `count` shown buttons and a body text.
function inspect(count, text, options) {
  const win = {getComputedStyle:() => ({visibility:"visible"}), frameElement:null};
  const doc = {nodeType:9, defaultView:win, title:"T", body:{innerText:text}};
  const els = Array.from({length:count}, (_, i) => ({nodeType:1, localName:"button", isConnected:true,
    ownerDocument:doc, shadowRoot:null, innerText:"Button number " + i, labels:[], getAttribute:() => null,
    getRootNode:() => doc, getClientRects:() => [{}], getBoundingClientRect:() => ({left:0, right:9, top:0, bottom:9}),
    matches:css => css !== "[draggable=true]"}));
  doc.querySelectorAll = () => els;
  const ctx = vm.createContext({document:doc, location:{origin:TOP, href:TOP + "/"}, innerWidth:800, innerHeight:600,
    TextEncoder, WeakRef, crypto:globalThis.crypto});
  vm.runInContext(sources[0], ctx);
  return plain(ctx.TrustyPageRead.pageRead(512, {scope_ref:null, viewport_only:false, ...options}));
}

test("include_text keeps a text share on an element-heavy page", () => {
  const body = "TOP-TEXT ".repeat(2000);
  const r = inspect(400, body, {max_chars:10000, include_text:true});
  // Before #11284 the text got only the slack the elements left, often nothing.
  assert.ok(r.text.startsWith("TOP-TEXT") && r.text.length >= 4000, `text kept ${r.text.length} chars`);
  assert.ok(r.elements.length > 0, "elements still fill the rest");
  assert.equal(r.truncated, true);
  assert.ok(new TextEncoder().encode(JSON.stringify(r)).length <= 10000);
  assert.deepEqual(Object.keys(r), ["origin", "url", "title", "text", "elements", "truncated"]);
  // Without include_text the elements take the whole budget and no text is read.
  const bare = inspect(400, body, {max_chars:10000, include_text:false});
  assert.equal(bare.text, "");
  assert.ok(bare.elements.length > r.elements.length);
});

test("include_text reads each granted child frame's text within its share", async () => {
  const b = browser({frames:page({text:"Child body CHILD-LABEL-7"}), allowed:[TOP + "/*", CHILD + "/*"]});
  const r = plain(await ask(commands(b), "page_read", {...READ, include_text:true})).result;
  assert.ok(r.text.includes("[frame 7]\nChild body CHILD-LABEL-7"), r.text);
  const child = b.calls.find(c => c.target.documentIds && c.func === "pageInspect");
  assert.equal(child.args[2].include_text, true);
  assert.equal(child.args[2].max_chars, 25000);
});

// #11808: the six actions on frame-scoped refs. Each runs a read first, as an agent would, so the worker has
// recorded the child's documentId under the lease's top document.
const lease = async (cmds, fields = READ) => plain(await ask(cmds, "page_read", fields)).result.document;
const act = (cmds, op, fields, document = "D0") => ask(cmds, op, {...fields, document});
const ran = (b, func) => plain(b.calls.filter(c => c.func === func && c.target.documentIds));
const form = (child = {}) => page({elements:[element("22.0", "CHILD-LABEL-7"), element("22.1", "Send")],
  ...child});

test("a granted child frame is typed into, submitted and read back", async () => {
  const b = browser({frames:form(), allowed:[TOP + "/*", CHILD + "/*"]});
  const cmds = commands(b);
  const top = await lease(cmds);
  const box = plain(await ask(cmds, "page_find", FIND)).result.matches[0];
  assert.equal(box.ref, "7:22.0");
  const typed = plain(await act(cmds, "page_type", {ref:box.ref, text:"hello\n"}, top));
  assert.deepEqual(typed.result, {document:"D0", frame:{id:7, origin:CHILD}});
  const keys = ran(b, "pageKeys");
  assert.deepEqual(keys.map(c => [c.target.documentIds, c.args]), [[["D7"], ["22.0", "type", "hello\n"]]]);
  const back = plain(await ask(cmds, "page_read", TEXT_READ)).result;
  assert.ok(back.text.includes("[frame 7]\nEcho: hello"), back.text);
  // The other actions reach the same child document with the child-local ref.
  const replies = [
    await act(cmds, "page_press", {ref:"7:22.0", key:"Enter", modifiers:[]}),
    await act(cmds, "page_select", {ref:"7:22.1", value:"x"}),
    await act(cmds, "page_hover", {ref:"7:22.1"}),
    await act(cmds, "page_scroll", {to:"element", ref:"7:22.1", wait_ms:0}),
  ].map(r => plain(r).result);
  assert.ok(replies.every(r => r.document === "D0" && r.frame.id === 7 && r.frame.origin === CHILD));
  assert.equal(replies[1].value, "x");
  for (const func of ["pageSelect", "pageHover", "pageScroll"]) {
    assert.deepEqual(ran(b, func).map(c => c.target.documentIds), [["D7"]], func);
  }
  // A top-document ref still runs in the top document and names no frame.
  assert.deepEqual(plain(await act(cmds, "page_hover", {ref:"11.0"})).result, {document:"D0"});
  assert.deepEqual(ran(b, "pageHover").at(-1).target.documentIds, ["D0"]);
});

test("an action on a child origin without permission is refused and never reaches the top", async () => {
  const allowed = [TOP + "/*", CHILD + "/*"];
  const b = browser({frames:form(), allowed});
  const cmds = commands(b);
  // No read recorded the child yet: the ref names nothing, and nothing runs.
  await failed(act(cmds, "page_type", {ref:"7:22.0", text:"x"}), "StaleElement");
  const top = await lease(cmds);
  allowed.splice(1);
  const ops = [["page_type", {text:"x"}], ["page_press", {key:"a", modifiers:[]}], ["page_select", {value:"x"}],
    ["page_hover", {}], ["page_scroll", {to:"element", wait_ms:0}], ["page_click", {dialog:"dismiss"}]];
  for (const [op, fields] of ops) {
    await assert.rejects(act(cmds, op, {ref:"7:22.0", ...fields}, top),
      e => e.message === "PermissionNeeded" && e.fields.origin === CHILD && e.fields.tab === 1, op);
  }
  const acting = ["pageKeys", "pageSelect", "pageHover", "pageScroll", "pageTarget"];
  assert.deepEqual(b.calls.filter(c => acting.includes(c.func)), [], "no frame and no top document was acted on");
  assert.deepEqual(b.input, [], "no trusted input was sent");
  // A child that was denied at read time never gets a ref, so a forged one names nothing.
  const denied = browser({frames:form()});
  const dc = commands(denied);
  const r = plain(await ask(dc, "page_read", READ)).result;
  assert.deepEqual(r.denied_frames, [{origin:CHILD, reason:"no_permission"}]);
  await failed(act(dc, "page_type", {ref:"7:22.0", text:"x"}, r.document), "StaleElement");
  assert.deepEqual(denied.calls.filter(c => acting.includes(c.func)), []);
});

test("a frame ref from an earlier top document or a navigated child is stale", async () => {
  const b = browser({frames:form(), allowed:[TOP + "/*", CHILD + "/*"]});
  const cmds = commands(b);
  await lease(cmds);
  // The lease names another top document: the worker refuses before touching any frame.
  await failed(act(cmds, "page_type", {ref:"7:22.0", text:"x"}, "D-old"), "DocumentChanged");
  b.gone.add("D7");
  await failed(act(cmds, "page_type", {ref:"7:22.0", text:"x"}), "StaleElement");
  // The record is dropped: a repeat answers StaleElement without injecting.
  const before = b.calls.length;
  await failed(act(cmds, "page_type", {ref:"7:22.0", text:"x"}), "StaleElement");
  assert.ok(!b.calls.slice(before).some(c => c.func === "pageKeys"));
});

test("a trusted click in a child frame lands at the top-frame point", async () => {
  const b = browser({frames:form(), allowed:[TOP + "/*", CHILD + "/*"]});
  const cmds = commands(b);
  const top = await lease(cmds);
  const reply = plain(await act(cmds, "page_click", {ref:"7:22.1", dialog:"dismiss"}, top));
  assert.deepEqual(reply.result, {document:"D0", frame:{id:7, origin:CHILD}});
  assert.deepEqual(ran(b, "pageTarget").map(c => [c.target.documentIds, c.args]), [[["D7"], ["22.1"]]]);
  const mouse = b.input.filter(([m]) => m === "Input.dispatchMouseEvent").map(([, p]) => [p.type, p.x, p.y]);
  assert.deepEqual(mouse, [["mouseMoved", 110, 220], ["mousePressed", 110, 220], ["mouseReleased", 110, 220]]);
  // The child's point is mapped through the owner's content quad, read from the top debugger session.
  const asked = b.input.map(([m]) => m);
  assert.ok(["DOM.getDocument", "DOM.getBoxModel", "DOM.getNodeForLocation"].every(m => asked.includes(m)), asked);
  // A scaled and rotated owner: the point follows the quad's axes (CSS pixels of the child map onto them).
  const turned = browser({frames:form(), allowed:[TOP + "/*", CHILD + "/*"],
    cdp:{quad:[100, 200, 300, 300, 250, 400, 50, 300]}});
  const tc = commands(turned);
  await act(tc, "page_click", {ref:"7:22.1", dialog:"dismiss"}, await lease(tc));
  const tp = turned.input.find(([m, p]) => m === "Input.dispatchMouseEvent" && p.type === "mousePressed")[1];
  assert.deepEqual([tp.x, tp.y], [Math.round(100 + 200 * 10 / 400 - 50 * 20 / 300),
    Math.round(200 + 100 * 10 / 400 + 100 * 20 / 300)]);
});

test("a click is refused when the owner is ambiguous, covered or unconfirmed", async () => {
  const pressed = b => b.input.some(([m]) => m === "Input.dispatchMouseEvent");
  const cases = [
    // Two owners load the child's URL: neither can be told from the other, so the page cannot steer the click.
    [{owners:[{node:50, frame:"F7", url:CHILD + "/"}, {node:51, frame:"F8", url:CHILD + "/#x"}]}, "Obscured"],
    // No owner loads it (the frame sits under another out-of-process frame).
    [{owners:[{node:51, frame:"F8", url:"https://c.test/"}]}, "Obscured"],
    // Another element covers the mapped point.
    [{at:{backendNodeId:99, frameId:"TOP"}}, "Obscured"],
    // A perspective transform leaves no affine map.
    [{quad:[100, 200, 500, 200, 520, 520, 90, 500]}, "Obscured"],
  ];
  for (const [cdp, error] of cases) {
    const b = browser({frames:form(), allowed:[TOP + "/*", CHILD + "/*"], cdp});
    const cmds = commands(b);
    await assert.rejects(act(cmds, "page_click", {ref:"7:22.1", dialog:"dismiss"}, await lease(cmds)),
      e => e.message === error && e.fields.frame?.id === 7 && e.fields.frame?.origin === CHILD, JSON.stringify(cdp));
    assert.ok(!pressed(b), "no input for " + JSON.stringify(cdp));
  }
  // The child saw no trusted pointer event reach the target: the click ran, so its outcome is unknown.
  const b = browser({frames:form(), allowed:[TOP + "/*", CHILD + "/*"], cdp:{hit:false}});
  const cmds = commands(b);
  await assert.rejects(act(cmds, "page_click", {ref:"7:22.1", dialog:"dismiss"}, await lease(cmds)),
    e => e.message === "OutcomeUnknown" && e.fields.frame?.id === 7);
  assert.ok(pressed(b));
  const reply = plain(cmds.failure({id:1, op:"page_click"}, await act(cmds, "page_click",
    {ref:"7:22.1", dialog:"dismiss"}).catch(e => e)));
  assert.deepEqual(reply, {id:1, ok:false, error:"OutcomeUnknown", frame:{id:7, origin:CHILD}});
});

test("a refused frame action names the frame it had resolved", async () => {
  const allowed = [TOP + "/*", CHILD + "/*"];
  const b = browser({frames:form(), allowed});
  const cmds = commands(b);
  const top = await lease(cmds);
  // A refusal from the child document itself.
  await assert.rejects(act(cmds, "page_select", {ref:"7:99.9", value:"x"}, top),
    e => e.message === "StaleElement" && e.fields.frame?.id === 7);
  const relayed = plain(cmds.failure({id:2, op:"page_select"},
    await act(cmds, "page_select", {ref:"7:99.9", value:"x"}, top).catch(e => e)));
  assert.deepEqual(relayed, {id:2, ok:false, error:"StaleElement", tab:1, frame:{id:7, origin:CHILD}});
  // A refusal before the frame resolved names none.
  await assert.rejects(act(cmds, "page_select", {ref:"8:1.0", value:"x"}, top),
    e => e.message === "StaleElement" && !("frame" in e.fields));
});
