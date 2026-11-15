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
        .map(role => "[role=" + role + "]").join(",") +
      ",[draggable=true],[aria-dropeffect]:not([aria-dropeffect=none]),[dropzone]";
    // #11288: a drop zone is often a bare div. When the page offers something to drag, elements whose
    // id or class names a drop target are reported too, so page_drag has a target ref.
    const dropNamed = document.querySelector("[draggable=true]") ?
      ",[id*=drop i],[class*=drop i],[data-drop],[data-dropzone]" : "";
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
    for (const el of document.querySelectorAll(selector + dropNamed)) {
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
      if (el.getAttribute("draggable") === "true") entry.draggable = true;
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

  // Why: a click must reach only an element this document's latest read reported.
  // What: resolves the ref, then dispatches synthetic pointer and mouse events and element.click().
  // Test: tests/test_pages_dom.py `test_click_and_type_refuse_stale_refs`.
  function pageClick(ref) {
    const state = globalThis.__trustyBrowserRefs, [read] = ref.split(".");
    const el = state && Number(read) === state.read ? state.map.get(ref)?.deref() : null;
    if (!el || !el.isConnected) return {error:"StaleElement"};
    el.scrollIntoView({block:"center", inline:"center"});
    const box = el.getBoundingClientRect();
    const at = {bubbles:true, cancelable:true, composed:true, view:window,
      clientX:box.left + box.width / 2, clientY:box.top + box.height / 2};
    el.dispatchEvent(new PointerEvent("pointerdown", at));
    el.dispatchEvent(new MouseEvent("mousedown", at));
    el.dispatchEvent(new PointerEvent("pointerup", at));
    el.dispatchEvent(new MouseEvent("mouseup", at));
    if (typeof el.focus === "function" && el.tabIndex >= 0) el.focus();
    el.click();
    return {ok:true};
  }

  // Why: a drag must start and end on elements this document's latest read reported (#11288).
  // What: dispatches dragstart, drag and dragend on the source and dragenter, dragover and drop on the
  //   target, sharing one DataTransfer. The drop fires only when the target cancels dragover (the HTML
  //   drop-target rule); otherwise dragleave fires. A cancelled dragstart ends the drag at once.
  // Test: tests/test_pages_dom.py `test_drag_fires_the_drag_sequence_and_drops`.
  function pageDrag(ref, target) {
    const state = globalThis.__trustyBrowserRefs;
    const resolve = r => {
      const el = state && Number(r.split(".")[0]) === state.read ? state.map.get(r)?.deref() : null;
      return el && el.isConnected ? el : null;
    };
    const source = resolve(ref), sink = resolve(target);
    if (!source || !sink) return {error:"StaleElement"};
    if (source === sink || source.contains(sink)) return {error:"NotDraggable"};
    const point = el => {
      el.scrollIntoView({block:"center", inline:"center"});
      const box = el.getBoundingClientRect();
      return {bubbles:true, cancelable:true, composed:true, view:window,
        clientX:box.left + box.width / 2, clientY:box.top + box.height / 2};
    };
    const data = new DataTransfer();
    data.effectAllowed = "all";
    const fire = (el, kind, at, cancelable = true) =>
      el.dispatchEvent(new DragEvent(kind, {...at, cancelable, dataTransfer:data}));
    const from = point(source);
    source.dispatchEvent(new PointerEvent("pointerdown", from));
    source.dispatchEvent(new MouseEvent("mousedown", from));
    if (!fire(source, "dragstart", from)) return {ok:true, dropped:false};
    fire(source, "drag", from);
    const to = point(sink);
    fire(sink, "dragenter", to);
    data.dropEffect = "move";
    const accepted = !fire(sink, "dragover", to);
    if (accepted) fire(sink, "drop", to);
    else {
      data.dropEffect = "none";
      fire(sink, "dragleave", to, false);
    }
    fire(source, "dragend", to, false);
    return {ok:true, dropped:accepted};
  }

  // Why: typing replaces the content of one supported editable element from the latest read.
  // What: native value setter plus bubbling input/change for input and textarea; insertText for
  //   contenteditable. Any other, disabled or read-only target answers NotEditable.
  // Test: tests/test_pages_dom.py `test_type_sets_values_and_refuses_non_editable`.
  function pageType(ref, text) {
    const state = globalThis.__trustyBrowserRefs, [read] = ref.split(".");
    const el = state && Number(read) === state.read ? state.map.get(ref)?.deref() : null;
    if (!el || !el.isConnected) return {error:"StaleElement"};
    const kinds = ["text","search","email","url","tel","password","number"];
    const field = el.localName === "textarea" ||
      (el.localName === "input" && kinds.includes((el.getAttribute("type") || "text").toLowerCase()));
    if ((!field && !el.isContentEditable) || el.disabled || el.readOnly) return {error:"NotEditable"};
    el.focus();
    if (field) {
      const proto = el.localName === "textarea" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, "value").set.call(el, text);
      el.dispatchEvent(new Event("input", {bubbles:true}));
      el.dispatchEvent(new Event("change", {bubbles:true}));
    } else {
      const range = document.createRange();
      range.selectNodeContents(el);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      if (text) document.execCommand("insertText", false, text); else document.execCommand("delete");
    }
    return {ok:true};
  }

  // URL floor: https any host, or http on 127.0.0.1; no credentials; at most 8192 characters.
  function urlAllowed(url) {
    if (typeof url !== "string" || !url || url.length > 8192) return false;
    let address;
    try { address = new URL(url); } catch { return false; }
    if (address.username || address.password) return false;
    return address.protocol === "https:" || (address.protocol === "http:" && address.hostname === "127.0.0.1");
  }
  return {pageProbe, pageRead, pageClick, pageType, pageDrag, urlAllowed};
})();

// Why: the broker's page commands must be confined by the worker alone, whatever Chrome would allow.
// What: validates each command in the contract's check order (op, fields, email, grant, scope, tab),
//   then runs it against the explicit tab id. No path reads "the active tab".
// Test: tests/test_worker.mjs `a tab grant never reaches another tab`, `stale document refuses before the page runs`.
TrustyPages.commands = deps => {
  const {pageProbe, pageRead, pageClick, pageType, pageDrag, urlAllowed} = TrustyPages;
  const RELAYED = ["tab","origin","access","document"];
  const REASONS = ["revoked","tab_closed","browser_gone","unenrolled","idle","audit","closed","expired"];
  const REF = /^[0-9]{1,9}\.[0-9]{1,6}$/;
  const tab = v => Number.isInteger(v) && v >= 0;
  const doc = v => v === null || (typeof v === "string" && v.length > 0 && v.length <= 8192);
  const ref = v => typeof v === "string" && REF.test(v);
  const SPECS = {
    page_tabs:{}, page_tab:{tab}, page_read:{tab, document:doc}, page_click:{tab, document:doc, ref},
    page_drag:{tab, document:doc, ref, target:ref},
    page_type:{tab, document:doc, ref, text:v => typeof v === "string" && v.length <= 8192},
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

  // Runs the injected click/type only inside the lease's document; never reports work it did not do.
  async function act(req, func, args) {
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
    if (["StaleElement","NotEditable","NotDraggable"].includes(error)) throw refuse(error, {tab:req.tab});
    if (hit?.result?.ok !== true || hit.documentId !== req.document) throw refuse("OutcomeUnknown");
    if (func !== pageDrag) return {document:req.document};
    if (typeof hit.result.dropped !== "boolean") throw refuse("OutcomeUnknown");
    return {document:req.document, dropped:hit.result.dropped};
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
    page_click:req => act(req, pageClick, [req.ref]),
    page_drag:req => act(req, pageDrag, [req.ref, req.target]),
    page_type:req => act(req, pageType, [req.ref, req.text]),
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
      const acting = ["page_click","page_type","page_drag","page_navigate"].includes(req.op);
      return {id:req.id, ok:false, error:acting ? "OutcomeUnknown" : "BrowserDisconnected"};
    }
    const fields = Object.fromEntries(Object.entries(error.fields || {}).filter(([k]) => RELAYED.includes(k)));
    return {id:req.id, ok:false, error:error.message, ...fields};
  }
  return {command, failure, probe, handles:op => Object.hasOwn(SPECS, op)};
};

