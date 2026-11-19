// Every frame of a leased tab over CDP, including cross-origin and out-of-process iframes (#11362).
// pages.js loads this file with importScripts. Top-level code touches no `chrome` API: each function takes
// the chrome.debugger object (or a frames context built from it) of a tab pages.js has already attached.
globalThis.TrustyFrames = (() => {
  // Chrome's built-in PDF viewer: another extension's frame, which chrome.debugger may not enter.
  const PDF_VIEWER = "chrome-extension://mhjfbmdgcfjbbpaeojofohoefgiehjai/";
  const AUTO = {autoAttach:true, waitForDebuggerOnStart:false, flatten:true};
  // #11362: the snapshot stays under the 1 MiB native frame (native_host.rs MAX_FRAME drops the whole bridge
  //   past it): at most NODE_BUDGET nodes, BYTE_BUDGET bytes of serialized nodes and frames, FRAME_MAX frames
  //   and DEPTH_MAX node depth, names cut to NAME_MAX. pages_frames.rs refuses only past 200 frames, depth 1000.
  const NODE_BUDGET = 2500, BYTE_BUDGET = 700000, FRAME_MAX = 150, DEPTH_MAX = 40, NAME_MAX = 160;
  // The UTF-8 length of `v` as JSON; a lone surrogate counts as its six-byte escape.
  const utf8 = v => {
    let n = 0;
    for (const ch of JSON.stringify(v)) { const c = ch.codePointAt(0); n += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4; }
    return n;
  };
  // Spends `bytes` from the shared budget; false (and truncated) when it would overrun.
  const spend = (budget, bytes) => {
    if (budget.bytes < bytes) { budget.truncated = true; budget.bytes = 0; return false; }
    budget.bytes -= bytes;
    return true;
  };
  const SKIP = new Set(["none", "generic", "InlineTextBox", "LineBreak"]);
  const ID = v => typeof v === "string" && v.length > 0 && v.length <= 64;
  const fail = (error, fields = {}) => Object.assign(new Error(error), {fields});

  // A frame-scoped ref as the broker sends it: the frame, its document's loader and the DOM backend node.
  const isRef = v => v !== null && typeof v === "object" && !Array.isArray(v) && Object.keys(v).length === 3 &&
    ID(v.frame) && ID(v.loader) && Number.isInteger(v.node) && v.node > 0 && v.node <= 1e12;

  // Why: an out-of-process iframe is its own CDP target; one chrome.debugger attachment reaches it only
  //   through flat child sessions, and none may outlive the operation (#11362).
  // What: on the tab pages.js attached, sends Target.setAutoAttach {autoAttach, flatten,
  //   waitForDebuggerOnStart:false}, and again in every iframe child session Chrome attaches, so nested
  //   out-of-process frames are reached too. Any other target (a worker, another extension's frame such as
  //   the PDF viewer) is detached at once; a PDF viewer is remembered by its parent frame. `ready` settles once
  //   no auto-attach is pending. `close` detaches every child session, deepest first, and removes the
  //   listener; after it, `send` refuses OutcomeUnknown and sends nothing. Attaches nothing itself.
  // Test: tests/js/frames.test.mjs `auto-attach reaches nested out-of-process frames and detaches other targets`,
  //   `every child session is detached before the tab, and nothing is sent after close`.
  function open(tabId, debug) {
    const sessions = new Map([[null, {id:null, parent:undefined, frame:null, parentFrame:null}]]);
    const pending = new Set(), pdf = new Set();
    let closed = false;
    const target = id => id === null ? {tabId} : {tabId, sessionId:id};
    const send = (id, method, params = {}) => closed ? Promise.reject(fail("OutcomeUnknown"))
      : debug.sendCommand(target(id), method, params);
    const track = promise => {
      const quiet = promise.catch(() => {});
      pending.add(quiet);
      quiet.then(() => pending.delete(quiet));
    };
    const listener = (source, method, params) => {
      if (source?.tabId !== tabId || closed) return;
      const parent = source.sessionId ?? null;
      if (method === "Target.attachedToTarget") {
        const info = params?.targetInfo || {}, id = params?.sessionId;
        if (!ID(id)) return;
        const url = String(info.url || "");
        if (info.type !== "iframe" || url.startsWith("chrome-extension://")) {
          if (url.startsWith(PDF_VIEWER) && ID(info.parentFrameId)) pdf.add(info.parentFrameId);
          track(send(parent, "Target.detachFromTarget", {sessionId:id}));
          return;
        }
        sessions.set(id, {id, parent, frame:String(info.targetId), parentFrame:ID(info.parentFrameId)
          ? info.parentFrameId : null});
        track(send(id, "Target.setAutoAttach", AUTO));
      } else if (method === "Target.detachedFromTarget") {
        for (const s of [...sessions.values()]) if (s.id !== null && s.id === params?.sessionId) sessions.delete(s.id);
      }
    };
    debug.onEvent.addListener(listener);
    const depth = s => { let n = 0; for (let at = s; at?.id !== null && at; at = sessions.get(at.parent)) n++; return n; };
    const ctx = {sessions, pdf, send,
      ready:(async () => {
        await send(null, "Target.setAutoAttach", AUTO);
        while (pending.size) await Promise.all([...pending]);
      })(),
      async close() {
        if (closed) return;
        closed = true;
        debug.onEvent.removeListener(listener);
        const children = [...sessions.values()].filter(s => s.id !== null).sort((a, b) => depth(b) - depth(a));
        for (const s of children) {
          await debug.sendCommand(target(s.parent), "Target.detachFromTarget", {sessionId:s.id}).catch(() => {});
          sessions.delete(s.id);
        }
      }};
    ctx.ready.catch(() => {});
    return ctx;
  }

  // The sessions root first, then each child after its parent.
  const ordered = ctx => {
    const out = [], seen = new Set([undefined]);
    while (out.length < ctx.sessions.size) {
      const next = [...ctx.sessions.values()].filter(s => !seen.has(s.id) && seen.has(s.parent));
      if (!next.length) break;
      for (const s of next) { out.push(s); seen.add(s.id); }
    }
    return out;
  };
  const owns = (ctx, frameId) => [...ctx.sessions.values()].some(s => s.id !== null && s.frame === frameId);
  const treeOf = async (ctx, s) => (await ctx.send(s.id, "Page.getFrameTree"))?.frameTree;

  // A frame owner's content box, in its session's local-root viewport CSS pixels; null when unreadable.
  const ownerBox = async (ctx, sessionId, frameId) => (await owner(ctx, sessionId, frameId))?.box ?? null;
  // A frame owner's backend node and content box ({node, box}); null when unreadable.
  async function owner(ctx, sessionId, frameId) {
    try {
      const {backendNodeId} = await ctx.send(sessionId, "DOM.getFrameOwner", {frameId});
      const q = (await ctx.send(sessionId, "DOM.getBoxModel", {backendNodeId}))?.model?.content;
      if (!Array.isArray(q) || q.length !== 8 || !q.every(Number.isFinite)) return null;
      const xs = [q[0], q[2], q[4], q[6]], ys = [q[1], q[3], q[5], q[7]];
      const x = Math.min(...xs), y = Math.min(...ys);
      return {node:backendNodeId, box:[x, y, Math.max(...xs) - x, Math.max(...ys) - y]};
    } catch { return null; }
  }

  // Why: an agent reads by role and name, so the snapshot keeps the browser's own accessibility tree,
  //   not a hand-written walker (#11362, F5, F8).
  // What: depth-first from the AX roots, drops ignored nodes and the SKIP roles (their children rise one
  //   level), keeps [depth, role, name, backendDOMNodeId, value] with name and value cut to NAME_MAX, and
  //   stops at the shared `budget` (nodes left, serialized bytes left), setting budget.truncated. A node
  //   deeper than DEPTH_MAX is dropped with its subtree, also setting budget.truncated.
  // Test: tests/js/frames.test.mjs `a snapshot reads every frame's accessibility tree within the node budget`,
  //   `a snapshot of 2500 CJK nodes and long URLs stays under the native frame and says it is truncated`.
  function compact(nodes, budget) {
    const byId = new Map(nodes.map(n => [n.nodeId, n])), out = [];
    const text = v => v === undefined || v === null ? null : String(v).replace(/\s+/g, " ").trim().slice(0, NAME_MAX);
    const visit = (n, depth) => {
      const role = String(n.role?.value ?? ""), name = text(n.name?.value) ?? "";
      const keep = !n.ignored && !SKIP.has(role) && !(role === "StaticText" && !name) &&
        Number.isInteger(n.backendDOMNodeId);
      if (keep) {
        if (depth > DEPTH_MAX || budget.left <= 0) { budget.truncated = true; return; }
        const row = [depth, role, name, n.backendDOMNodeId, text(n.value?.value)];
        if (!spend(budget, utf8(row) + 1)) return;
        budget.left--;
        out.push(row);
      }
      for (const c of n.childIds || []) if (byId.has(c)) visit(byId.get(c), depth + (keep ? 1 : 0));
    };
    for (const n of nodes) if (!byId.has(n.parentId)) visit(n, 0);
    return out;
  }

  // Why: the agent must read every frame of its tab, the Omni embed included (#11362).
  // What: after `ready`, walks Page.getFrameTree in every session (a child session's frames hang under the
  //   frame its target names, else its parent session's root); a frame another session owns is listed once,
  //   by that session. Per frame: its loader, URL, security origin, whether it is its session's root (an
  //   out-of-process frame), its owner box in the parent's local-root viewport, and Accessibility.getFullAXTree
  //   compacted. A PDF document or the PDF viewer is listed refused PdfViewer with no nodes, a tree Chrome will
  //   not give FrameUnreachable. The top frame's tree failing throws. Past FRAME_MAX frames or the byte budget
  //   (each frame's fields count, its nodes through compact) it stops and answers truncated: true.
  // Test: tests/js/frames.test.mjs `a snapshot reads every frame's accessibility tree within the node budget`,
  //   `the pdf viewer and an unreadable frame are listed refused with a reason`,
  //   `a snapshot of 2500 CJK nodes and long URLs stays under the native frame and says it is truncated`.
  async function snapshot(ctx) {
    await ctx.ready;
    const frames = [], budget = {left:NODE_BUDGET, bytes:BYTE_BUDGET, truncated:false};
    for (const s of ordered(ctx)) {
      let tree;
      try { tree = await treeOf(ctx, s); } catch (error) { if (s.id === null) throw error; continue; }
      if (!tree?.frame) { if (s.id === null) throw fail("OutcomeUnknown"); continue; }
      const parentRoot = s.id === null ? null : (await treeOf(ctx, ctx.sessions.get(s.parent)).catch(() => null));
      const walk = async (node, parent, root) => {
        const f = node.frame;
        if (!root && owns(ctx, f.id)) return;
        if (frames.length >= FRAME_MAX) { budget.truncated = true; return; }
        const ownerSession = root ? s.parent : s.id;
        const entry = {frame:String(f.id), parent, loader:String(f.loaderId ?? ""), url:String(f.url ?? "").slice(0, 2048),
          origin:String(f.securityOrigin ?? ""), root:root && s.id !== null,
          owner:parent === null ? null : await ownerBox(ctx, ownerSession, f.id), refused:null, nodes:[]};
        if (!spend(budget, utf8(entry) + 32)) return; // 32: room for the `refused` reason
        if (f.mimeType === "application/pdf" || ctx.pdf.has(f.id)) entry.refused = "PdfViewer";
        else {
          try {
            const ax = await ctx.send(s.id, "Accessibility.getFullAXTree", {frameId:f.id});
            entry.nodes = compact(Array.isArray(ax?.nodes) ? ax.nodes : [], budget);
          } catch { entry.refused = "FrameUnreachable"; }
        }
        frames.push(entry);
        for (const c of node.childFrames || []) await walk(c, entry.frame, false);
      };
      await walk(tree, s.id === null ? null : s.parentFrame ?? String(parentRoot?.frame?.id ?? ""), true);
    }
    return {frames, truncated:budget.truncated};
  }

  // Why: a ref must act only in the document it was read from (#11362).
  // What: finds the frame in the session that owns it (its own out-of-process session first); no such frame
  //   is StaleElement (detached), another loader is DocumentChanged (navigated).
  // Test: tests/js/frames.test.mjs `a navigated frame answers DocumentChanged and a detached one StaleElement`.
  async function locate(ctx, ref) {
    await ctx.ready;
    const find = (node, id) => node?.frame?.id === id ? node.frame
      : (node?.childFrames || []).reduce((hit, c) => hit || find(c, id), null);
    const order = ordered(ctx).sort((a, b) => Number(b.frame === ref.frame) - Number(a.frame === ref.frame));
    for (const s of order) {
      let tree;
      try { tree = await treeOf(ctx, s); } catch { continue; }
      const hit = find(tree, ref.frame);
      if (!hit || (s.frame !== ref.frame && owns(ctx, ref.frame))) continue;
      if (hit.loaderId !== ref.loader) throw fail("DocumentChanged");
      return s.id;
    }
    throw fail("StaleElement");
  }

  // Why: CDP gives a node's box in its own local root's viewport, while Input events take the top viewport.
  // What: the sum, up the session chain, of each out-of-process frame owner's content-box origin in its
  //   parent session; {x:0, y:0} for the top session. An owner it cannot read answers StaleElement.
  // Test: tests/js/frames.test.mjs `a click inside a nested out-of-process frame lands at the summed offset`.
  async function offset(ctx, sessionId) {
    let x = 0, y = 0;
    for (let s = ctx.sessions.get(sessionId); s && s.id !== null; s = ctx.sessions.get(s.parent)) {
      const box = await ownerBox(ctx, s.parent, s.frame);
      if (!box) throw fail("StaleElement");
      x += box[0];
      y += box[1];
    }
    return {x, y};
  }

  // The node's first content quad, scrolled into view, as [left, top, width, height] in its local viewport.
  async function quad(ctx, id, node) {
    let q;
    try {
      await ctx.send(id, "DOM.getDocument", {depth:0});
      await ctx.send(id, "DOM.scrollIntoViewIfNeeded", {backendNodeId:node});
      q = (await ctx.send(id, "DOM.getContentQuads", {backendNodeId:node}))?.quads?.[0];
    } catch { throw fail("StaleElement"); }
    if (!Array.isArray(q) || q.length !== 8 || !q.every(Number.isFinite)) throw fail("StaleElement");
    const xs = [q[0], q[2], q[4], q[6]], ys = [q[1], q[3], q[5], q[7]];
    return [Math.min(...xs), Math.min(...ys), Math.max(...xs) - Math.min(...xs), Math.max(...ys) - Math.min(...ys)];
  }

  // Why: a trusted click on a frame ref must land on that element, in top-viewport CSS pixels (#11362); an
  //   overlay in a parent document over the iframe would take the click instead.
  // What: locates the frame, scrolls the node into view, takes its content-quad centre and hit-tests it in the
  //   frame (DOM.getNodeForLocation). Then, per out-of-process level up to the top, adds the frame owner's
  //   content-box origin and hit-tests that point in the parent session: the hit must be the owner element or
  //   inside it. Any other hit at any level answers Obscured.
  // Test: tests/js/frames.test.mjs `a click inside a nested out-of-process frame lands at the summed offset`,
  //   `a covered frame element answers Obscured`, `an overlay over the frame in its parent document answers Obscured`.
  async function point(ctx, ref) {
    const id = await locate(ctx, ref);
    const [left, top, width, height] = await quad(ctx, id, ref.node);
    let x = left + width / 2, y = top + height / 2;
    await hitTest(ctx, id, ref.frame, ref.node, x, y);
    for (let s = ctx.sessions.get(id); s && s.id !== null; s = ctx.sessions.get(s.parent)) {
      const own = await owner(ctx, s.parent, s.frame);
      if (!own) throw fail("StaleElement");
      x += own.box[0];
      y += own.box[1];
      const frame = s.parentFrame ?? (await treeOf(ctx, ctx.sessions.get(s.parent)).catch(() => null))?.frame?.id;
      await hitTest(ctx, s.parent, frame, own.node, x, y);
    }
    return {ok:true, x, y};
  }
  // The element at (x, y) of session `id` is `node` or inside it, else Obscured.
  async function hitTest(ctx, id, frame, node, x, y) {
    const hit = await ctx.send(id, "DOM.getNodeForLocation", {x:Math.round(x), y:Math.round(y),
      includeUserAgentShadowDOM:false}).catch(() => null);
    if (hit?.backendNodeId !== node && !(await inside(ctx, id, frame, node, hit?.backendNodeId))) throw fail("Obscured");
  }
  // Why: page script can replace Node.prototype.contains in its main world, so the check runs elsewhere (#11362).
  // What: resolves both nodes in a fresh isolated world of `frame` and calls contains there; any failure is false.
  // Test: tests/js/frames.test.mjs `the covered-element check resolves both nodes in an isolated world, not the page's`.
  async function inside(ctx, id, frame, node, other) {
    if (!Number.isInteger(other) || typeof frame !== "string") return false;
    try {
      const world = await isolated(ctx, id, frame);
      const outer = (await ctx.send(id, "DOM.resolveNode", {backendNodeId:node, executionContextId:world}))?.object?.objectId;
      const inner = (await ctx.send(id, "DOM.resolveNode", {backendNodeId:other, executionContextId:world}))?.object?.objectId;
      if (typeof outer !== "string" || typeof inner !== "string") return false;
      const r = await ctx.send(id, "Runtime.callFunctionOn", {objectId:outer, returnByValue:true,
        functionDeclaration:"function(n){return Node.prototype.contains.call(this, n)}", arguments:[{objectId:inner}]});
      return r?.result?.value === true;
    } catch { return false; }
  }
  // A new isolated world of `frame` in session `id`, as its execution context id; throws FrameScriptBlocked.
  async function isolated(ctx, id, frame) {
    let world;
    try {
      world = (await ctx.send(id, "Page.createIsolatedWorld", {frameId:frame, worldName:"trusty-browser",
        grantUniveralAccess:false}))?.executionContextId;
    } catch { throw fail("FrameScriptBlocked"); }
    if (!Number.isInteger(world)) throw fail("FrameScriptBlocked");
    return world;
  }

  // #11211, #11362: a frame ref's box in top-viewport CSS pixels, for an element screenshot.
  async function region(ctx, ref) {
    const id = await locate(ctx, ref);
    const [left, top, width, height] = await quad(ctx, id, ref.node);
    const at = await offset(ctx, id);
    return {x:left + at.x, y:top + at.y, width, height};
  }

  // Why: hover, type, press, scroll, select, upload and drag must behave in a frame exactly as in the top
  //   document, so they run the same injected page function (#11362).
  // What: locates the frame, creates an isolated world in it (refused FrameScriptBlocked when Chrome will not),
  //   resolves the node (and `other`, which must sit in the same frame, else NotDraggable) into that world,
  //   then calls `func` with `this` bound to a wrapper that answers the read registry: "@el" is the node,
  //   "@other" the second node. Returns the page function's own result value; a thrown page error is
  //   OutcomeUnknown.
  // Test: tests/js/frames.test.mjs `a frame ref runs the page function in an isolated world of its frame`,
  //   `a frame whose isolated world is refused answers FrameScriptBlocked`.
  async function run(ctx, ref, other, func, args) {
    const id = await locate(ctx, ref);
    if (other && other.frame !== ref.frame) throw fail("NotDraggable");
    const world = await isolated(ctx, id, ref.frame);
    const resolve = async node => {
      try {
        const objectId = (await ctx.send(id, "DOM.resolveNode", {backendNodeId:node, executionContextId:world}))
          ?.object?.objectId;
        if (typeof objectId === "string") return objectId;
      } catch { /* refused below */ }
      throw fail("StaleElement");
    };
    const self = await resolve(ref.node), second = other ? await resolve(other.node) : null;
    const functionDeclaration = `function(args, other) { const el = this, prev = globalThis.__trustyBrowserRefs;
      globalThis.__trustyBrowserRefs = {resolve:r => r === "@el" ? el : r === "@other" ? other : null};
      try { return (${func.toString()})(...args); } finally { globalThis.__trustyBrowserRefs = prev; } }`;
    let reply;
    try {
      reply = await ctx.send(id, "Runtime.callFunctionOn", {objectId:self, functionDeclaration, awaitPromise:true,
        returnByValue:true, arguments:[{value:args}, second ? {objectId:second} : {value:null}]});
    } catch { throw fail("OutcomeUnknown"); }
    if (reply?.exceptionDetails) throw fail("OutcomeUnknown");
    return reply?.result?.value;
  }

  return {isRef, open, compact, snapshot, locate, offset, point, region, run, NODE_BUDGET};
})();
