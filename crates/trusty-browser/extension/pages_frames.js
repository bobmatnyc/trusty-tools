// Actions inside cross-origin child frames (#11808, docs/activation-codes.md "Cross-origin child frames").
// pages.js loads this file (importScripts in the worker; a vm source in tests). Top-level code touches no
// `chrome` API. The `page*` functions are injected by chrome.scripting.executeScript, so each one is
// self-contained: Chrome serializes the function source and runs it in the frame's isolated world.
globalThis.TrustyFrameActs = (() => {
  // Why: a trusted click is aimed from geometry, and a page can move, cover or transform its frames, so the
  //   child document must confirm the click really reached the target (#11808 review).
  // What: resolves `ref` (StaleElement when it changed) and installs isolated-world capture listeners on the
  //   element's own window for trusted pointerdown, mousedown and click events whose path includes the
  //   element; pageWitnessed reads and removes them. Reads no page content.
  // Test: tests/extension/test_frames_uat.py `test_click_in_a_granted_child_frame_lands_trusted`,
  //   `test_click_is_not_redirected_by_a_page_relaying_messages`.
  function pageWitness(ref, nonce) {
    const el = globalThis.__trustyBrowserRefs?.resolve?.(ref);
    if (!el) return {error:"StaleElement"};
    const view = el.ownerDocument.defaultView, seen = {hit:false};
    const listen = event => { if (event.isTrusted && event.composedPath().includes(el)) seen.hit = true; };
    for (const type of ["pointerdown", "mousedown", "click"]) view.addEventListener(type, listen, true);
    (globalThis.__trustyWitness ??= new Map()).set(nonce, {view, seen, listen});
    return {ok:true};
  }
  function pageWitnessed(nonce) {
    const entry = globalThis.__trustyWitness?.get(nonce);
    if (!entry) return {ok:true, hit:false};
    globalThis.__trustyWitness.delete(nonce);
    for (const type of ["pointerdown", "mousedown", "click"]) entry.view.removeEventListener(type, entry.listen, true);
    return {ok:true, hit:entry.seen.hit};
  }

  // #11808: the frame-scoped ref grammar a read hands out (pages.js FRAME_REF).
  const FRAME_REF = /^([0-9]{1,9}):([0-9]{1,9}\.[0-9]{1,6})$/;
  const bare = url => String(url ?? "").split("#")[0];
  // The <iframe>/<frame> owners the top debugger session sees: the top document and same-process documents.
  function owners(node, out = []) {
    if ((node?.nodeName === "IFRAME" || node?.nodeName === "FRAME") && typeof node.frameId === "string") {
      out.push({node:node.backendNodeId, frame:node.frameId});
    }
    for (const child of [...(node?.children ?? []), ...(node?.shadowRoots ?? [])]) owners(child, out);
    if (node?.contentDocument) owners(node.contentDocument, out);
    return out;
  }
  function urls(tree, out = new Map()) {
    if (tree?.frame) out.set(tree.frame.id, tree.frame.url);
    for (const child of tree?.childFrames ?? []) urls(child, out);
    return out;
  }

  // Why: an action on a frame-scoped ref must run only in the child document a read reported, only while the
  //   lease's top document is current and the extension holds host permission for the child's own origin
  //   (#11808); it must never fall back to the top frame, and its refusals must name that child.
  // What: `of(req)` answers null for a top-document ref. For a frame ref it re-probes the top (DocumentChanged
  //   when it moved), looks up the child a read recorded under the same top document (StaleElement when none),
  //   re-checks the child origin's permission (PermissionNeeded naming the frame) and answers
  //   {id, local, document, origin}. `tag` adds `frame` to any later refusal. `lost` turns a failed injection
  //   into StaleElement (the child is gone; nothing ran) or OutcomeUnknown. `locate` maps a child point to
  //   top-frame CSS pixels through CDP: the one owner whose frame URL is the child's, its transformed content
  //   quad, and a CDP hit test; any doubt answers Obscured. `watch`/`witnessed` confirm a trusted pointer event
  //   reached the target in the child.
  // Test: tests/extension/test_frames.mjs `a granted child frame is typed into, submitted and read back`,
  //   `an action on a child origin without permission is refused and never reaches the top`,
  //   `a trusted click in a child frame lands at the top-frame point`,
  //   `a click is refused when the owner is ambiguous, covered or unconfirmed`.
  function worker({refuse, probe, granted, frameDocs, frameKey, pageProbe}) {
    const named = frame => ({id:frame.id, origin:frame.origin});
    async function of(req) {
      const match = typeof req.ref === "string" ? FRAME_REF.exec(req.ref) : null;
      if (!match) return null;
      const now = await probe(req.tab);
      if (!now.access || now.document !== req.document) {
        throw refuse("DocumentChanged", {tab:req.tab, access:now.access, document:now.document});
      }
      const known = frameDocs.get(frameKey(req.tab, match[1]));
      if (!known || known.top !== req.document) throw refuse("StaleElement", {tab:req.tab});
      const frame = {id:Number(match[1]), local:match[2], document:known.document, origin:known.origin};
      if (!await granted(known.origin)) {
        throw refuse("PermissionNeeded", {tab:req.tab, origin:known.origin, frame:named(frame)});
      }
      return frame;
    }
    // A refusal raised after the frame resolved names it; an unexpected exception is OutcomeUnknown.
    function tag(error, frame) {
      if (!frame) return error;
      if (!error?.fields) return refuse("OutcomeUnknown", {frame:named(frame)});
      error.fields.frame = named(frame);
      return error;
    }
    async function lost(req, frame) {
      try {
        await chrome.scripting.executeScript({target:{tabId:req.tab, documentIds:[frame.document]}, func:pageProbe});
      } catch {
        frameDocs.delete(frameKey(req.tab, frame.id));
        return refuse("StaleElement", {tab:req.tab});
      }
      return refuse("OutcomeUnknown");
    }
    async function locate(req, frame, point, cdp) {
      const blocked = () => refuse("Obscured", {tab:req.tab});
      const want = bare(point.url);
      if (!want || !(point.vw > 0) || !(point.vh > 0)) throw blocked();
      let found;
      try { found = owners((await cdp("DOM.getDocument", {depth:-1, pierce:true})).root); } catch { throw blocked(); }
      let known = new Map();
      try { known = urls((await cdp("Page.getFrameTree")).frameTree); } catch { /* OOPIF targets below. */ }
      // An out-of-process frame is its own debugger target whose id is its frame id; the CDP Target domain is
      //   closed to extensions, so chrome.debugger.getTargets lists them.
      try {
        for (const t of await chrome.debugger.getTargets()) {
          if (typeof t?.id === "string" && !known.has(t.id)) known.set(t.id, t.url);
        }
      } catch { /* Only same-process frames are known. */ }
      // Two owners with the child's URL cannot be told apart, so neither is clicked.
      const hits = found.filter(o => bare(known.get(o.frame)) === want);
      if (hits.length !== 1) throw blocked();
      const [owner] = hits;
      await cdp("DOM.scrollIntoViewIfNeeded", {backendNodeId:owner.node,
        rect:{x:point.x, y:point.y, width:1, height:1}}).catch(() => {});
      let q;
      try { q = (await cdp("DOM.getBoxModel", {backendNodeId:owner.node})).model.content; } catch { throw blocked(); }
      if (!Array.isArray(q) || q.length !== 8 || !q.every(Number.isFinite)) throw blocked();
      // A perspective transform leaves no parallelogram, and no affine map from the child's viewport.
      if (Math.abs(q[0] + q[4] - q[2] - q[6]) > 1 || Math.abs(q[1] + q[5] - q[3] - q[7]) > 1) throw blocked();
      const fx = point.x / point.vw, fy = point.y / point.vh;
      const x = Math.round(q[0] + (q[2] - q[0]) * fx + (q[6] - q[0]) * fy);
      const y = Math.round(q[1] + (q[3] - q[1]) * fx + (q[7] - q[1]) * fy);
      let at;
      try {
        at = await cdp("DOM.getNodeForLocation", {x, y, includeUserAgentShadowDOM:true, ignorePointerEventsNone:true});
      } catch { throw blocked(); }
      if (at?.backendNodeId !== owner.node && at?.frameId !== owner.frame) throw blocked();
      return {x, y};
    }
    async function watch(req, frame) {
      const nonce = crypto.randomUUID();
      let hit;
      try {
        [hit] = await chrome.scripting.executeScript({target:{tabId:req.tab, documentIds:[frame.document]},
          func:pageWitness, args:[frame.local, nonce]});
      } catch { throw await lost(req, frame); }
      if (hit?.result?.error === "StaleElement") throw refuse("StaleElement", {tab:req.tab});
      if (hit?.result?.ok !== true) throw refuse("OutcomeUnknown");
      return nonce;
    }
    async function witnessed(req, frame, nonce) {
      try {
        const [hit] = await chrome.scripting.executeScript({target:{tabId:req.tab, documentIds:[frame.document]},
          func:pageWitnessed, args:[nonce]});
        return hit?.result?.hit === true;
      } catch { return false; }
    }
    return {of, tag, lost, locate, watch, witnessed};
  }

  return {pageWitness, pageWitnessed, FRAME_REF, worker};
})();
