// Actions inside cross-origin child frames (#11808, docs/activation-codes.md "Cross-origin child frames").
// pages.js loads this file (importScripts in the worker; a script tag or vm source in tests). Top-level code
// touches no `chrome` API. The `page*` functions are injected by chrome.scripting.executeScript, so each one is
// self-contained: Chrome serializes the function source and runs it in the frame's isolated world.
globalThis.TrustyFrameActs = (() => {
  // Why: a trusted click needs top-frame coordinates, and a cross-origin child cannot see its own iframe
  //   element (#11808). A message's `source` is the sending frame's window, which the parent can match
  //   against each iframe's `contentWindow`; that names the exact iframe element.
  // What: installs a one-shot isolated-world `message` listener in this frame for `nonce`. On a probe whose
  //   source is one of this document's iframes (open shadow roots included), it records that iframe and
  //   acknowledges to the sender. Reads no page content.
  // Test: tests/extension/test_frames_uat.py `test_click_in_a_granted_child_frame_lands_trusted`.
  function pageFrameHost(nonce) {
    const hosts = globalThis.__trustyFrameHosts ??= new Map();
    const owned = () => {
      const out = [], roots = [document];
      while (roots.length) {
        for (const el of roots.pop().querySelectorAll("*")) {
          if (el.localName === "iframe" || el.localName === "frame") out.push(el);
          if (el.shadowRoot) roots.push(el.shadowRoot);
        }
      }
      return out;
    };
    const entry = {frame:null, listen:null};
    entry.listen = event => {
      if (entry.frame || event.data?.trustyFrameProbe !== nonce) return;
      const frame = owned().find(f => f.contentWindow === event.source);
      if (!frame) return;
      entry.frame = frame;
      removeEventListener("message", entry.listen, true);
      event.source.postMessage({trustyFrameAck:nonce}, "*");
    };
    addEventListener("message", entry.listen, true);
    hosts.set(nonce, entry);
    return true;
  }

  // Why: the child announces itself to its parent so pageFrameHost can find its iframe element.
  // What: posts `{trustyFrameProbe: nonce}` to the parent window and resolves true once the parent
  //   acknowledges, false after `waitMs` or in a top frame.
  // Test: tests/extension/test_frames_uat.py `test_click_in_a_granted_child_frame_lands_trusted`.
  function pageFramePost(nonce, waitMs) {
    if (window.parent === window) return false;
    return new Promise(resolve => {
      let timer = null;
      const listen = event => {
        if (event.source === window.parent && event.data?.trustyFrameAck === nonce) done(true);
      };
      function done(value) {
        clearTimeout(timer);
        removeEventListener("message", listen, true);
        resolve(value);
      }
      timer = setTimeout(() => done(false), waitMs);
      addEventListener("message", listen, true);
      window.parent.postMessage({trustyFrameProbe:nonce}, "*");
    });
  }

  // Why: the point a child's pageTarget returned is in the child's viewport; CDP input takes the top's.
  // What: in the frame whose pageFrameHost matched `nonce`, adds the iframe's content-box offset to `point`
  //   (scrolling the iframe into view first when the point lies outside this viewport) and hit-tests the result
  //   up through open shadow roots: any other element on top answers Obscured. Every other frame drops its
  //   listener and answers null.
  // Test: tests/extension/test_frames_uat.py `test_click_in_a_granted_child_frame_lands_trusted`.
  function pageFrameOwner(nonce, point) {
    const hosts = globalThis.__trustyFrameHosts, entry = hosts?.get(nonce);
    if (!entry) return null;
    hosts.delete(nonce);
    removeEventListener("message", entry.listen, true);
    const frame = entry.frame;
    if (!frame?.isConnected) return null;
    const at = () => {
      const box = frame.getBoundingClientRect(), css = getComputedStyle(frame);
      return {x:box.left + frame.clientLeft + parseFloat(css.paddingLeft) + point.x,
        y:box.top + frame.clientTop + parseFloat(css.paddingTop) + point.y};
    };
    const outside = p => p.x < 0 || p.y < 0 || p.x >= innerWidth || p.y >= innerHeight;
    let p = at();
    if (outside(p)) {
      frame.scrollIntoView({block:"center", inline:"center", behavior:"instant"});
      p = at();
      if (outside(p)) scrollBy({left:p.x - innerWidth / 2, top:p.y - innerHeight / 2, behavior:"instant"});
      p = at();
    }
    for (let node = frame; ;) {
      const root = node.getRootNode(), hit = root.elementFromPoint?.(p.x, p.y);
      if (!hit || !(hit === node || node.contains(hit))) return {error:"Obscured"};
      if (!root.host) return {ok:true, x:p.x, y:p.y};
      node = root.host;
    }
  }

  // #11808: the frame-scoped ref grammar a read hands out (pages.js FRAME_REF).
  const FRAME_REF = /^([0-9]{1,9}):([0-9]{1,9}\.[0-9]{1,6})$/;
  // Nested frames a click may cross on its way to the top; deeper answers PermissionNeeded.
  const MAX_HOPS = 8, ACK_MS = 1000;

  // Why: an action on a frame-scoped ref must run only in the child document a read reported, only while the
  //   lease's top document is current and the extension holds host permission for the child's own origin
  //   (#11808); it must never fall back to the top frame.
  // What: `of(req)` answers null for a top-document ref. For a frame ref it re-probes the top (DocumentChanged
  //   when it moved), looks up the child a read recorded under the same top document (StaleElement when none),
  //   re-checks the child origin's permission (PermissionNeeded naming that origin) and answers
  //   {id, local, document, origin}. `lost` turns a failed injection into StaleElement (the child document is
  //   gone; nothing ran) or OutcomeUnknown. `locate` lifts a child point to top-frame coordinates, one parent
  //   hop at a time.
  // Test: tests/extension/test_frames.mjs `a granted child frame is typed into, submitted and read back`,
  //   `an action on a child origin without permission is refused and never reaches the top`,
  //   `a trusted click in a child frame lands at the top-frame point`.
  function worker({refuse, probe, granted, frameDocs, frameKey, pageProbe}) {
    async function of(req) {
      const match = typeof req.ref === "string" ? FRAME_REF.exec(req.ref) : null;
      if (!match) return null;
      const now = await probe(req.tab);
      if (!now.access || now.document !== req.document) {
        throw refuse("DocumentChanged", {tab:req.tab, access:now.access, document:now.document});
      }
      const known = frameDocs.get(frameKey(req.tab, match[1]));
      if (!known || known.top !== req.document) throw refuse("StaleElement", {tab:req.tab});
      if (!await granted(known.origin)) throw refuse("PermissionNeeded", {tab:req.tab, origin:known.origin});
      return {id:Number(match[1]), local:match[2], document:known.document, origin:known.origin};
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
    const unreachable = req => refuse("PermissionNeeded", {tab:req.tab});
    async function locate(req, frame, point) {
      const exec = async (target, func, args) => {
        try { return await chrome.scripting.executeScript({target:{tabId:req.tab, ...target}, func, args}); } catch {
          throw unreachable(req);
        }
      };
      let doc = frame.document, at = {x:point.x, y:point.y};
      for (let hop = 0; hop < MAX_HOPS; hop++) {
        const nonce = crypto.randomUUID();
        await exec({allFrames:true}, pageFrameHost, [nonce]);
        const [posted] = await exec({documentIds:[doc]}, pageFramePost, [nonce, ACK_MS]);
        const hits = await exec({allFrames:true}, pageFrameOwner, [nonce, at]);
        if (posted?.result !== true) throw unreachable(req);
        const owners = (Array.isArray(hits) ? hits : []).filter(h => h?.result);
        if (owners.length !== 1) throw unreachable(req);
        const [owner] = owners;
        if (owner.result.error === "Obscured") throw refuse("Obscured", {tab:req.tab});
        if (owner.result.ok !== true || !Number.isFinite(owner.result.x) || !Number.isFinite(owner.result.y) ||
            typeof owner.documentId !== "string") throw refuse("OutcomeUnknown");
        at = {x:owner.result.x, y:owner.result.y};
        if (owner.frameId === 0) {
          if (owner.documentId !== req.document) throw refuse("OutcomeUnknown");
          return at;
        }
        doc = owner.documentId;
      }
      throw unreachable(req);
    }
    return {of, lost, locate};
  }

  return {pageFrameHost, pageFramePost, pageFrameOwner, FRAME_REF, worker};
})();
