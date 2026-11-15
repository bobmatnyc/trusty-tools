// Page operations for activation-code grants (#11239, docs/activation-codes.md).
// The worker loads this file with importScripts; tests load it into a page. Top-level code touches no
// `chrome` API. The `page*` functions are injected by chrome.scripting.executeScript, so each one
// is self-contained: Chrome serializes the function source and runs it in the tab's isolated world.
globalThis.TrustyPages = (() => {
  // Returns [origin, title] and changes nothing; the InjectionResult supplies the documentId.
  function pageProbe() { return [location.origin, document.title]; }

  // Why: an agent addresses elements by refs from its own latest read, never by selectors.
  // What: walks the top document's light DOM, keeps ref -> WeakRef(element) in an isolated-world map that
  //   replaces any earlier map, and returns the read result bounded to 262144 UTF-8 bytes.
  // Test: tests/test_pages_dom.py `test_read_reports_elements_and_bounds_size`.
  function pageRead(reserve) {
    const LIMIT = 262144 - reserve, KEY = "__trustyBrowserRefs";
    const state = globalThis[KEY] || (globalThis[KEY] = {read:0, map:new Map()});
    state.read += 1;
    const read = state.read, map = new Map();
    let truncated = false;
    const cut = (value, max) => {
      const text = String(value ?? "");
      if (text.length <= max) return text;
      truncated = true;
      return text.slice(0, max);
    };
    const clean = value => String(value ?? "").replace(/\s+/g, " ").trim();
    const selector = "a[href],button,input:not([type=hidden]),select,textarea,summary," +
      "[contenteditable]:not([contenteditable=false])," +
      ["button","link","checkbox","radio","tab","menuitem","option","switch","textbox","combobox","searchbox"]
        .map(role => "[role=" + role + "]").join(",");
    function implicitRole(el) {
      const tag = el.localName, type = (el.getAttribute("type") || "text").toLowerCase();
      if (tag === "a") return "link";
      if (tag === "button" || tag === "summary") return "button";
      if (tag === "select") return el.multiple || el.size > 1 ? "listbox" : "combobox";
      if (tag === "textarea" || el.isContentEditable) return "textbox";
      if (tag === "input") {
        if (type === "checkbox" || type === "radio") return type;
        if (type === "search") return "searchbox";
        if (["button","submit","reset","image"].includes(type)) return "button";
        return "textbox";
      }
      return "generic";
    }
    function accessibleName(el) {
      const labelled = (el.getAttribute("aria-labelledby") || "").split(/\s+/).filter(Boolean)
        .map(id => document.getElementById(id)?.innerText || "").join(" ");
      const labels = [...(el.labels || [])].map(label => label.innerText).join(" ");
      for (const value of [el.getAttribute("aria-label"), labelled, labels, el.getAttribute("placeholder"),
        el.innerText, el.getAttribute("title"), el.getAttribute("alt")]) {
        const name = clean(value);
        if (name) return name.slice(0, 200);
      }
      return "";
    }
    const elements = [];
    for (const el of document.querySelectorAll(selector)) {
      if (!el.getClientRects().length || getComputedStyle(el).visibility !== "visible") continue;
      if (elements.length === 500) { truncated = true; break; }
      const ref = read + "." + elements.length, tag = el.localName;
      const entry = {ref, tag, role:el.getAttribute("role") || implicitRole(el), name:accessibleName(el)};
      if (tag === "input") entry.type = (el.getAttribute("type") || "text").toLowerCase();
      if (["input","textarea","select"].includes(tag) && entry.type !== "password" &&
          !["checkbox","radio"].includes(entry.type)) entry.value = cut(el.value, 512);
      if (tag === "a" && el.href) entry.href = cut(el.href, 2048);
      if (entry.type === "checkbox" || entry.type === "radio") entry.checked = el.checked;
      if (el.disabled === true || el.getAttribute("aria-disabled") === "true") entry.disabled = true;
      elements.push(entry);
      map.set(ref, new WeakRef(el));
    }
    const result = {origin:location.origin, url:cut(location.href, 2048), title:cut(document.title, 200),
      text:cut(document.body ? document.body.innerText : "", 65536), elements, truncated};
    const size = () => new TextEncoder().encode(JSON.stringify(result)).length;
    for (let over = size() - LIMIT; over > 0; over = size() - LIMIT) {
      result.truncated = true;
      if (result.text) result.text = result.text.slice(0, Math.max(0, result.text.length - over));
      else map.delete(result.elements.pop().ref);
    }
    result.truncated = result.truncated || truncated;
    state.map = map;
    return result;
  }

  // Why: a trusted click or keystroke must land only on an element this document's latest read reported
  //   (#11294); the worker then drives real input at the point this returns.
  // What: resolves the ref, checks a "type" target is editable, scrolls it to the viewport centre, and for
  //   "type" focuses it and selects its content so the first keystroke replaces it. A "click" whose centre
  //   another element covers answers Obscured. Returns the centre in top-frame CSS pixels.
  // Test: tests/test_pages_dom.py `test_target_refuses_stale_refs_and_covered_points`,
  //   `test_type_sets_values_and_refuses_non_editable`.
  function pageTarget(ref, kind) {
    const state = globalThis.__trustyBrowserRefs, [read] = ref.split(".");
    const el = state && Number(read) === state.read ? state.map.get(ref)?.deref() : null;
    if (!el || !el.isConnected) return {error:"StaleElement"};
    const kinds = ["text","search","email","url","tel","password","number"];
    const field = el.localName === "textarea" ||
      (el.localName === "input" && kinds.includes((el.getAttribute("type") || "text").toLowerCase()));
    if (kind === "type" && ((!field && !el.isContentEditable) || el.disabled || el.readOnly)) return {error:"NotEditable"};
    el.scrollIntoView({block:"center", inline:"center"});
    const box = el.getBoundingClientRect(), x = box.left + box.width / 2, y = box.top + box.height / 2;
    if (kind === "click") {
      const hit = document.elementFromPoint(x, y);
      return hit && (hit === el || el.contains(hit)) ? {ok:true, x, y} : {error:"Obscured"};
    }
    el.focus();
    if (field) el.select();
    else {
      const range = document.createRange();
      range.selectNodeContents(el);
      getSelection().removeAllRanges();
      getSelection().addRange(range);
    }
    return {ok:true, x, y};
  }

  // Why: offscreen content and infinite-scroll feeds load only when the page scrolls, and an agent needs
  //   to know when a feed has stopped growing (#11291).
  // What: `to` "down"/"up" scrolls the document by 90% of the viewport, "top"/"bottom" to an end, and
  //   "element" brings `ref` (from the latest read, else StaleElement) to the centre. It then waits up to
  //   `waitMs` for added nodes, returning 150 ms after they stop. The view reports the scroll position,
  //   whether the document grew and whether it now sits at the bottom. Refs stay valid: no read runs.
  // Test: tests/test_pages_dom.py `test_scroll_drives_an_infinite_feed_until_it_ends`,
  //   `test_scroll_to_element_and_stale_refs_after_dom_change`.
  async function pageScroll(to, ref, waitMs) {
    let el = null;
    if (to === "element") {
      const state = globalThis.__trustyBrowserRefs, [read] = ref.split(".");
      el = state && Number(read) === state.read ? state.map.get(ref)?.deref() : null;
      if (!el || !el.isConnected) return {error:"StaleElement"};
    }
    const root = document.scrollingElement || document.documentElement, before = root.scrollHeight;
    let added = false, quiet = null, finish;
    const done = new Promise(resolve => { finish = resolve; });
    const observer = new MutationObserver(records => {
      if (!records.some(r => r.addedNodes.length)) return;
      added = true;
      clearTimeout(quiet);
      quiet = setTimeout(finish, 150);
    });
    observer.observe(document, {childList:true, subtree:true});
    const limit = setTimeout(finish, waitMs);
    const step = Math.round(innerHeight * 0.9);
    if (to === "element") el.scrollIntoView({block:"center", inline:"center"});
    else if (to === "down") scrollBy(0, step);
    else if (to === "up") scrollBy(0, -step);
    else if (to === "top") scrollTo(0, 0);
    else scrollTo(0, root.scrollHeight);
    if (waitMs > 0) await done;
    observer.disconnect();
    clearTimeout(limit);
    clearTimeout(quiet);
    const y = Math.round(scrollY), height = root.scrollHeight;
    return {ok:true, view:{scroll_y:y, scroll_height:height, viewport_height:innerHeight,
      grew:added && height > before, at_end:y + innerHeight >= height - 2}};
  }
  // Why: sites that need native input or user activation reject synthetic events (#11294).
  // What: drives Chrome DevTools Protocol input through `send(method, params)`, so every event the page
  //   sees is trusted. A click is move, press, release at the point. Typing sends one keyDown/keyUp per
  //   code point (Enter for a newline, insertText for other control characters); empty text sends
  //   Backspace over the selection pageTarget made.
  // Test: tests/test_pages_dom.py `test_trusted_click_and_type_arrive_trusted`.
  const trusted = {
    async click(send, {x, y}) {
      const at = {x, y, button:"left", clickCount:1};
      await send("Input.dispatchMouseEvent", {type:"mouseMoved", x, y});
      await send("Input.dispatchMouseEvent", {type:"mousePressed", ...at, buttons:1});
      await send("Input.dispatchMouseEvent", {type:"mouseReleased", ...at, buttons:0});
    },
    async type(send, text) {
      const key = async down => {
        await send("Input.dispatchKeyEvent", {type:"keyDown", ...down});
        await send("Input.dispatchKeyEvent", {type:"keyUp", key:down.key, code:down.code,
          windowsVirtualKeyCode:down.windowsVirtualKeyCode});
      };
      if (!text) return key({key:"Backspace", code:"Backspace", windowsVirtualKeyCode:8});
      for (const ch of text) {
        if (ch === "\n") await key({key:"Enter", code:"Enter", windowsVirtualKeyCode:13, text:"\r", unmodifiedText:"\r"});
        else if (ch < " " || ch === "\u007f") await send("Input.insertText", {text:ch});
        else await key({key:ch, text:ch, unmodifiedText:ch});
      }
    },
    // A native dialog blocks the page until answered: an alert is acknowledged, any other follows `policy`.
    accepts:(type, policy) => type === "alert" || policy === "accept",
    // #11294: a key event carrying Ctrl or Meta can reach browser shortcuts (close tab, new tab, address
    //   bar), outside the lease's document. The worker sends no such event; this refuses one if it ever does.
    keyAllowed:(method, params) => method !== "Input.dispatchKeyEvent" || ((params?.modifiers ?? 0) & 6) === 0,
  };

  // URL floor: https any host, or http on 127.0.0.1; no credentials; at most 8192 characters.
  function urlAllowed(url) {
    if (typeof url !== "string" || !url || url.length > 8192) return false;
    let address;
    try { address = new URL(url); } catch { return false; }
    if (address.username || address.password) return false;
    return address.protocol === "https:" || (address.protocol === "http:" && address.hostname === "127.0.0.1");
  }
  return {pageProbe, pageRead, pageTarget, pageScroll, trusted, urlAllowed};
})();

// Why: the broker's page commands must be confined by the worker alone, whatever Chrome would allow.
// What: validates each command in the contract's check order (op, fields, email, grant, scope, tab),
//   then runs it against the explicit tab id. No path reads "the active tab".
// Test: tests/test_worker.mjs `a tab grant never reaches another tab`, `stale document refuses before the page runs`.
TrustyPages.commands = deps => {
  const {pageProbe, pageRead, pageTarget, pageScroll, trusted, urlAllowed} = TrustyPages;
  const RELAYED = ["tab","origin","access","document"];
  const REASONS = ["revoked","tab_closed","browser_gone","unenrolled","idle","audit","closed","expired"];
  const REF = /^[0-9]{1,9}\.[0-9]{1,6}$/;
  const tab = v => Number.isInteger(v) && v >= 0;
  const doc = v => v === null || (typeof v === "string" && v.length > 0 && v.length <= 8192);
  const ref = v => typeof v === "string" && REF.test(v);
  const SCROLL_TO = ["down","up","top","bottom","element"];
  const dialog = v => v === "accept" || v === "dismiss";
  const SPECS = {
    page_tabs:{}, page_tab:{tab}, page_read:{tab, document:doc}, page_click:{tab, document:doc, ref, dialog},
    page_type:{tab, document:doc, ref, dialog, text:v => typeof v === "string" && v.length <= 8192},
    // #11291: `ref` is a string exactly when `to` is "element"; `wait_ms` bounds the wait for new content.
    page_scroll:{tab, document:doc, to:v => SCROLL_TO.includes(v), ref:v => v === null || ref(v),
      wait_ms:v => Number.isInteger(v) && v >= 0 && v <= 5000},
    page_navigate:{tab, url:v => typeof v === "string" && v.length > 0 && v.length <= 8192},
    grant_end:{reason:v => REASONS.includes(v), last:v => typeof v === "boolean"},
  };
  const refuse = (error, fields = {}) => Object.assign(new Error(error), {fields});
  const none = {access:false, document:null, origin:null, title:null};

  async function probe(tabId) {
    let hit;
    try {
      [hit] = await chrome.scripting.executeScript({target:{tabId, frameIds:[0]}, func:pageProbe});
    } catch { return none; }
    if (!Array.isArray(hit?.result) || typeof hit.documentId !== "string" || typeof hit.result[0] !== "string") return none;
    await deps.remember(tabId, hit.result[0]);
    return {access:true, document:hit.documentId, origin:hit.result[0], title:String(hit.result[1]).slice(0, 200)};
  }
  async function needed(tabId) { return refuse("PermissionNeeded", {tab:tabId, origin:await deps.originOf(tabId)}); }

  function settle(tabId) {
    let finish;
    const done = new Promise(resolve => {
      let loading = false;
      const listener = (id, info) => {
        if (id !== tabId) return;
        if (info.status === "loading") loading = true;
        if (loading && info.status === "complete") finish(true);
      };
      const timer = setTimeout(() => finish(false), 5000);
      finish = value => { clearTimeout(timer); chrome.tabs.onUpdated.removeListener(listener); resolve(value); };
      chrome.tabs.onUpdated.addListener(listener);
    });
    return {done, cancel:() => finish(false)};
  }

  // Runs an injected scroll only inside the lease's document; never reports work it did not do.
  async function inject(req, func, args) {
    if (req.document === null) {
      const now = await probe(req.tab);
      throw refuse("DocumentChanged", {tab:req.tab, access:now.access, document:now.document});
    }
    let hit;
    try {
      [hit] = await chrome.scripting.executeScript({target:{tabId:req.tab, documentIds:[req.document]}, func, args});
    } catch {
      const now = await probe(req.tab);
      // The lease's document is still current, so the failure came from the page: the outcome is unknown.
      if (now.access && now.document === req.document) throw refuse("OutcomeUnknown");
      throw refuse("DocumentChanged", {tab:req.tab, access:now.access, document:now.document});
    }
    const error = hit?.result?.error;
    if (error === "StaleElement") throw refuse(error, {tab:req.tab});
    if (hit?.result?.ok !== true || hit.documentId !== req.document) throw refuse("OutcomeUnknown");
    return {document:req.document, ...(hit.result.view || {})};
  }

  // Why: a click or type must arrive trusted, yet run only inside the lease's document and never report
  //   work it did not do (#11294).
  // What: probes first and attaches chrome.debugger only to an accessible tab still on the lease's document
  //   (before targeting, since Chrome's debugging bar resizes the viewport), injects pageTarget into that
  //   document only, then drives CDP input at the point it returns. Before every input event it re-probes
  //   and refuses DocumentChanged if the document or origin moved, and refuses a Ctrl/Meta key event. Any
  //   native dialog is answered by `req.dialog` (an alert is always acknowledged); an answer Chrome
  //   rejects, or a command outlasting its time bound, answers OutcomeUnknown. It waits 100 ms in the page
  //   for a late dialog, then always detaches. The reply names the last dialog seen.
  // Test: tests/test_worker.mjs `click and type drive trusted input and answer dialogs`,
  //   `stale document refuses before the page runs`, `trusted input rechecks the document before every event`,
  //   `a no-access tab is never debugger-attached`, `a failed dialog answer or a hung command answers OutcomeUnknown`,
  //   `ctrl or meta key events never reach the tab`.
  async function act(req, kind, drive) {
    const changed = now => refuse("DocumentChanged", {tab:req.tab, access:now.access, document:now.document});
    if (req.document === null) throw changed(await probe(req.tab));
    const first = await probe(req.tab);
    if (!first.access || first.document !== req.document) throw changed(first);
    const debuggee = {tabId:req.tab}, raw = (method, params = {}) => chrome.debugger.sendCommand(debuggee, method, params);
    // Every input event runs only while the lease's document and origin are still the tab's top document.
    const send = async (method, params = {}) => {
      if (method.startsWith("Input.")) {
        if (!trusted.keyAllowed(method, params)) throw refuse("InvalidRequest");
        // An open dialog blocks the page probe, so its answer settles first.
        if (!(await answered)) throw refuse("OutcomeUnknown");
        const now = await probe(req.tab);
        if (!now.access || now.document !== req.document || now.origin !== first.origin) throw changed(now);
      }
      return raw(method, params);
    };
    try { await chrome.debugger.attach(debuggee, "1.3"); } catch {
      const now = await probe(req.tab);
      if (now.access && now.document === req.document) throw refuse("DebuggerBusy", {tab:req.tab});
      throw changed(now);
    }
    let seen = null, answered = Promise.resolve(true), timer;
    const answer = (source, method, params) => {
      if (source.tabId !== req.tab || method !== "Page.javascriptDialogOpening") return;
      const accept = trusted.accepts(params?.type, req.dialog);
      seen = {type:String(params?.type), message:String(params?.message ?? "").slice(0, 1024), accepted:accept};
      answered = raw("Page.handleJavaScriptDialog", {accept}).then(() => true, () => false);
    };
    chrome.debugger.onEvent.addListener(answer);
    const bound = new Promise((_, reject) => {
      timer = setTimeout(() => reject(refuse("OutcomeUnknown")), 5000 + 50 * (req.text?.length ?? 0));
    });
    try {
      await Promise.race([bound, steps()]);
    } finally {
      clearTimeout(timer);
      chrome.debugger.onEvent.removeListener(answer);
      await chrome.debugger.detach(debuggee).catch(() => {});
    }
    return seen ? {document:req.document, dialog:seen} : {document:req.document};

    async function steps() {
      await raw("Page.enable");
      let hit;
      try {
        [hit] = await chrome.scripting.executeScript({target:{tabId:req.tab, documentIds:[req.document]},
          func:pageTarget, args:[req.ref, kind]});
      } catch {
        const now = await probe(req.tab);
        // The lease's document is still current, so the failure came from the page: the outcome is unknown.
        if (now.access && now.document === req.document) throw refuse("OutcomeUnknown");
        throw changed(now);
      }
      const point = hit?.result, error = point?.error;
      if (["StaleElement","NotEditable","Obscured"].includes(error)) throw refuse(error, {tab:req.tab});
      if (point?.ok !== true || hit.documentId !== req.document || !Number.isFinite(point.x) ||
          !Number.isFinite(point.y)) throw refuse("OutcomeUnknown");
      await drive(send, point);
      await raw("Runtime.evaluate", {expression:"new Promise(done => setTimeout(done, 100))", awaitPromise:true})
        .catch(() => {});
      // A dialog Chrome would not close leaves the page blocked: the outcome is not the one reported.
      if (!(await answered)) throw refuse("OutcomeUnknown");
    }
  }

  const run = {
    async page_tabs(req, row) {
      let tabs;
      if (row.scope === "tab") {
        try { tabs = [await chrome.tabs.get(row.tab)]; } catch { throw refuse("TabNotFound", {tab:row.tab}); }
      } else tabs = await chrome.tabs.query({});
      const entries = await Promise.all(tabs.map(async t => {
        const seen = await probe(t.id);
        return {tab:t.id, window:t.windowId, index:t.index, active:t.active === true,
          access:seen.access, origin:seen.origin, title:seen.title};
      }));
      return {tabs:entries};
    },
    page_tab:req => probe(req.tab),
    async page_read(req) {
      let hit;
      try {
        [hit] = await chrome.scripting.executeScript({target:{tabId:req.tab, frameIds:[0]}, func:pageRead, args:[512]});
      } catch { throw await needed(req.tab); }
      const result = hit?.result;
      if (!result || typeof hit.documentId !== "string" || typeof result.origin !== "string") throw await needed(req.tab);
      await deps.remember(req.tab, result.origin);
      return {document:hit.documentId, origin:result.origin, url:result.url, title:result.title,
        text:result.text, elements:result.elements, truncated:result.truncated};
    },
    page_click:req => act(req, "click", trusted.click),
    page_type:req => act(req, "type", send => trusted.type(send, req.text)),
    page_scroll(req) {
      if ((req.to === "element") !== (req.ref !== null)) throw refuse("InvalidRequest");
      return inject(req, pageScroll, [req.to, req.ref, req.wait_ms]);
    },
    async page_navigate(req) {
      if (!urlAllowed(req.url)) throw refuse("InvalidUrl");
      const wait = settle(req.tab);
      try { await chrome.tabs.update(req.tab, {url:req.url}); } catch {
        wait.cancel();
        throw refuse("TabNotFound", {tab:req.tab});
      }
      await deps.remember(req.tab, new URL(req.url).origin);
      const settled = await wait.done;
      const {access, document, origin} = await probe(req.tab);
      return {access, document, origin, settled};
    },
  };

  async function command(req) {
    const spec = SPECS[req.op];
    if (!spec) throw refuse("InvalidOperation");
    const keys = ["id","op","email","grant", ...Object.keys(spec)];
    if (Object.keys(req).length !== keys.length || !keys.every(k => Object.hasOwn(req, k)) ||
        typeof req.email !== "string" || typeof req.grant !== "string" || !req.grant ||
        !Object.entries(spec).every(([k, ok]) => ok(req[k]))) throw refuse("InvalidRequest");
    if (await deps.identity() !== req.email) throw refuse("ProfileMismatch");
    if (req.op === "grant_end") {
      // grant_end refuses only a malformed frame or another account; an unknown grant is already ended.
      try { await deps.ended(req.grant, req.last); } catch { /* The next acknowledgement prunes the row. */ }
      return {id:req.id, ok:true, email:req.email, result:"ended"};
    }
    const row = await deps.grant(req.grant);
    if (!row) throw refuse("UnknownGrant");
    if ("tab" in spec) {
      if (row.scope === "tab" && req.tab !== row.tab) throw refuse("OutOfScope", {tab:req.tab});
      try { await chrome.tabs.get(req.tab); } catch { throw refuse("TabNotFound", {tab:req.tab}); }
    }
    const result = await run[req.op](req, row);
    return {id:req.id, ok:true, email:req.email, result, ...("tab" in spec ? {tab:req.tab} : {})};
  }
  // A refusal carries only the fields the contract lets accompany an error.
  // An unexpected exception never relays Chrome's text (it can carry a URL): a command that may have
  // changed the page answers OutcomeUnknown, any other BrowserDisconnected.
  function failure(req, error) {
    if (!error?.fields) {
      const acting = ["page_click","page_type","page_scroll","page_navigate"].includes(req.op);
      return {id:req.id, ok:false, error:acting ? "OutcomeUnknown" : "BrowserDisconnected"};
    }
    const fields = Object.fromEntries(Object.entries(error.fields || {}).filter(([k]) => RELAYED.includes(k)));
    return {id:req.id, ok:false, error:error.message, ...fields};
  }
  return {command, failure, probe, handles:op => Object.hasOwn(SPECS, op)};
};

