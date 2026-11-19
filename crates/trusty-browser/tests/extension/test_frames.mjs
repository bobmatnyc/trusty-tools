// Cross-origin child-frame reads in page_read / page_find (#11301), against the shipped extension in
// crates/trusty-browser/extension with mocked chrome APIs. Run: node --test crates/trusty-browser/tests/extension/
import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const dir = new URL("../../extension/", import.meta.url);
const sources = await Promise.all(["pages_read.js", "pages.js"].map(f => readFile(new URL(f, dir), "utf8")));
const email = "qa@example.org";
const READ = {scope_ref:null, max_chars:50000, viewport_only:false, include_text:false};
const FIND = {role:null, name:"CHILD-LABEL", text:null, within_ref:null, limit:10};
const TOP = "https://a.test", CHILD = "https://b.test", GRAND = "https://c.test";

const element = (ref, name) => ({ref, tag:"button", role:"button", name});
const read = (origin, elements) => ({origin, url:origin + "/", title:"T", text:"", elements, truncated:false});
const found = (origin, matches) => ({origin, matches:matches.map(m => ({...m, in:{role:"form", name:"", ref:"1.9"}})),
  truncated:false});

// A tab whose frames are `frames` ({id, origin, document, root, children, elements}); frame 0 is the top.
// `allowed` holds the granted host patterns; `gone` the documentIds whose frame navigated away.
function browser({frames, allowed = [TOP + "/*"], injected = null}) {
  const calls = [], gone = new Set();
  const byDoc = new Map(frames.map(f => [f.document, f]));
  const contains = ({origins:[pattern]}) => allowed.includes(pattern) || allowed.includes("https://*/*");
  const reachable = f => (injected ?? (f => contains({origins:[new URL(f.origin).protocol + "//" +
    new URL(f.origin).hostname + "/*"]})))(f);
  const answer = (f, func, args) => {
    if (func.name === "pageProbe") return [f.origin, "T", f.origin + "/"];
    if (func.name === "pageFrames") return {origin:f.origin, url:f.origin + "/", root:f.root ?? f.id !== 0,
      children:f.children ?? []};
    const [mode, , options] = args;
    const scope = options.scope_ref ?? options.within_ref;
    if (scope != null && !f.elements.some(e => e.ref === scope)) return {error:"StaleElement"};
    const list = scope != null ? f.elements.filter(e => e.ref !== scope) : f.elements;
    return mode === "read" ? read(f.origin, list) : found(f.origin, list.filter(e => e.name.includes(options.name)));
  };
  const chrome = {
    tabs:{get:async id => ({id, status:"complete"}), onUpdated:{addListener(){}, removeListener(){}}},
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
  return {ctx, chrome, calls, gone};
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
  const b = browser({frames:page()});
  const r = plain(await ask(commands(b), "page_read", READ)).result;
  assert.deepEqual(r.denied_frames, [{origin:CHILD, reason:"no_permission"}]);
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"}]);
  assert.ok(!JSON.stringify(r).includes("CHILD-LABEL"));
  assert.ok(!b.calls.some(c => c.target.documentIds), "no child document was read");
  // activeTab-style access: Chrome injected the child, yet the extension holds no permission for its origin.
  const loose = browser({frames:page(), injected:() => true});
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
  // A frame ref is never an action target: click and type in frames are not in this build (#11318).
  await failed(ask(cmds, "page_click", {ref:"7:22.0", dialog:"dismiss"}), "InvalidRequest");
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
  const b = browser({frames:page({origin:"null"}), allowed:["https://*/*"], injected:() => true});
  const r = plain(await ask(commands(b), "page_read", READ)).result;
  assert.deepEqual(r.denied_frames, [{origin:"null", reason:"opaque"}]);
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"}]);
  assert.ok(!JSON.stringify(r).includes("CHILD-LABEL"));
  assert.ok(!b.calls.some(c => c.target.documentIds), "no child document was injected");
});

test("a child that navigated between discovery and the pinned read is denied as navigated", async () => {
  const allowed = [TOP + "/*", CHILD + "/*"];
  // The document is gone by the time the pinned read runs.
  const gone = browser({frames:page(), allowed});
  gone.gone.add("D7");
  const r = plain(await ask(commands(gone), "page_read", READ)).result;
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"}]);
  assert.deepEqual(r.denied_frames, [{origin:CHILD, reason:"navigated"}]);
  assert.ok(!JSON.stringify(r).includes("CHILD-LABEL"));
  assert.ok(gone.calls.some(c => c.target.documentIds), "the pinned read was attempted");
  // The same document answers from another origin.
  const moved = browser({frames:page({later:GRAND}), allowed:[...allowed, GRAND + "/*"]});
  const f = plain(await ask(commands(moved), "page_find", FIND)).result;
  assert.deepEqual(f.matches, []);
  assert.deepEqual(f.denied_frames, [{origin:CHILD, reason:"navigated"}]);
  assert.ok(!JSON.stringify(f).includes("CHILD-LABEL"));
});

test("a failing permission check denies the child with no content", async () => {
  const b = browser({frames:page(), allowed:["https://*/*"], injected:() => true});
  b.chrome.permissions.contains = async () => { throw new Error("permissions unavailable"); };
  const r = plain(await ask(commands(b), "page_read", READ)).result;
  assert.deepEqual(r.denied_frames, [{origin:CHILD, reason:"no_permission"}]);
  assert.deepEqual(r.frames, [{id:0, origin:TOP, url:TOP + "/", document:"D0"}]);
  assert.ok(!JSON.stringify(r).includes("CHILD-LABEL"));
  assert.ok(!b.calls.some(c => c.target.documentIds));
});
