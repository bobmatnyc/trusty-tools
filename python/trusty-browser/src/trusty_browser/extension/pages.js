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
          !["checkbox","radio","file"].includes(entry.type)) entry.value = cut(el.value, 512);
      // #11289: a file input reports the names it holds, never Chrome's C:\fakepath value.
      if (entry.type === "file") {
        entry.files = [...(el.files || [])].slice(0, 20).map(file => cut(file.name, 255));
        if (el.multiple) entry.multiple = true;
        if (el.accept) entry.accept = cut(el.accept, 256);
      }
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

  // Why: a file input cannot be typed into; the agent must hand the page real file bytes (#11289).
  // What: builds File objects from base64 data, sets them through a DataTransfer, then fires bubbling
  //   input and change as a user's pick would. A target that is not an enabled <input type=file>, or more
  //   than one file for a single-file input, answers NotEditable and changes nothing.
  // Test: tests/test_pages_dom.py `test_set_files_reaches_the_page_and_the_server`,
  //   `test_set_files_refuses_other_targets`.
  function pageSetFiles(ref, files) {
    const state = globalThis.__trustyBrowserRefs, [read] = ref.split(".");
    const el = state && Number(read) === state.read ? state.map.get(ref)?.deref() : null;
    if (!el || !el.isConnected) return {error:"StaleElement"};
    const isFile = el.localName === "input" && (el.getAttribute("type") || "").toLowerCase() === "file";
    if (!isFile || el.disabled || (files.length > 1 && !el.multiple)) return {error:"NotEditable"};
    const transfer = new DataTransfer();
    for (const {name, type, data} of files) {
      const raw = atob(data), bytes = new Uint8Array(raw.length);
      for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
      transfer.items.add(new File([bytes], name, {type}));
    }
    el.files = transfer.files;
    el.dispatchEvent(new Event("input", {bubbles:true, composed:true}));
    el.dispatchEvent(new Event("change", {bubbles:true}));
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
  return {pageProbe, pageRead, pageClick, pageType, pageScroll, pageSetFiles, urlAllowed};
})();

// Why: the broker's page commands must be confined by the worker alone, whatever Chrome would allow.
// What: validates each command in the contract's check order (op, fields, email, grant, scope, tab),
//   then runs it against the explicit tab id. No path reads "the active tab".
// Test: tests/test_worker.mjs `a tab grant never reaches another tab`, `stale document refuses before the page runs`.
TrustyPages.commands = deps => {
  const {pageProbe, pageRead, pageClick, pageType, pageScroll, pageSetFiles, urlAllowed} = TrustyPages;
  const RELAYED = ["tab","origin","access","document"];
  const REASONS = ["revoked","tab_closed","browser_gone","unenrolled","idle","audit","closed","expired"];
  const REF = /^[0-9]{1,9}\.[0-9]{1,6}$/;
  const tab = v => Number.isInteger(v) && v >= 0;
  const doc = v => v === null || (typeof v === "string" && v.length > 0 && v.length <= 8192);
  const ref = v => typeof v === "string" && REF.test(v);
  const SCROLL_TO = ["down","up","top","bottom","element"];
  // #11289: at most 10 files and 700000 base64 characters, so a frame stays under the 1 MiB native limit.
  const B64 = /^[A-Za-z0-9+/]*={0,2}$/;
  const files = v => Array.isArray(v) && v.length >= 1 && v.length <= 10 &&
    v.reduce((sum, f) => sum + (typeof f?.data === "string" ? f.data.length : Infinity), 0) <= 700000 &&
    v.every(f => f && typeof f === "object" && Object.keys(f).length === 3 &&
      typeof f.name === "string" && f.name.length > 0 && f.name.length <= 255 && !/[\\/]/.test(f.name) &&
      typeof f.type === "string" && f.type.length <= 255 &&
      typeof f.data === "string" && f.data.length % 4 === 0 && B64.test(f.data));
  const SPECS = {
    page_tabs:{}, page_tab:{tab}, page_read:{tab, document:doc}, page_click:{tab, document:doc, ref},
    page_type:{tab, document:doc, ref, text:v => typeof v === "string" && v.length <= 8192},
    // #11291: `ref` is a string exactly when `to` is "element"; `wait_ms` bounds the wait for new content.
    page_scroll:{tab, document:doc, to:v => SCROLL_TO.includes(v), ref:v => v === null || ref(v),
      wait_ms:v => Number.isInteger(v) && v >= 0 && v <= 5000},
    page_navigate:{tab, url:v => typeof v === "string" && v.length > 0 && v.length <= 8192},
    page_upload:{tab, document:doc, ref, files}, page_downloads:{tab},
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

  // Runs the injected click/type/scroll only inside the lease's document; never reports work it did not do.
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
    if (error === "StaleElement" || error === "NotEditable") throw refuse(error, {tab:req.tab});
    if (hit?.result?.ok !== true || hit.documentId !== req.document) throw refuse("OutcomeUnknown");
    return {document:req.document, ...(hit.result.view || {})};
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
    page_type:req => act(req, pageType, [req.ref, req.text]),
    page_scroll(req) {
      if ((req.to === "element") !== (req.ref !== null)) throw refuse("InvalidRequest");
      return act(req, pageScroll, [req.to, req.ref, req.wait_ms]);
    },
    page_upload:req => act(req, pageSetFiles, [req.ref, req.files]),
    // Why: a download the page starts must be visible to the agent, never another site's (#11289).
    // What: lists the 20 newest Chrome downloads whose referrer, URL or final URL has the tab's current
    //   origin (a blob: URL counts as its creator's origin). Needs page access; without the downloads
    //   permission it answers InvalidOperation.
    // Test: tests/test_worker.mjs `downloads are listed only for the tab's origin`.
    async page_downloads(req) {
      if (typeof chrome.downloads?.search !== "function") throw refuse("InvalidOperation");
      const seen = await probe(req.tab);
      if (!seen.access) throw await needed(req.tab);
      const originOf = url => { try { return new URL(url).origin; } catch { return null; } };
      const items = await chrome.downloads.search({orderBy:["-startTime"], limit:200});
      const downloads = items
        .filter(d => [d.referrer, d.url, d.finalUrl].some(u => u && originOf(u) === seen.origin))
        .slice(0, 20)
        .map(d => ({id:d.id, path:String(d.filename || "").slice(0, 4096),
          url:String(d.finalUrl || d.url || "").slice(0, 2048), mime:String(d.mime || "").slice(0, 255),
          state:String(d.state || ""), bytes:Number(d.bytesReceived) || 0,
          total:Number(d.totalBytes) || 0, error:d.error ? String(d.error) : null,
          started:String(d.startTime || "")}));
      return {origin:seen.origin, downloads};
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
      const acting = ["page_click","page_type","page_scroll","page_navigate","page_upload"].includes(req.op);
      return {id:req.id, ok:false, error:acting ? "OutcomeUnknown" : "BrowserDisconnected"};
    }
    const fields = Object.fromEntries(Object.entries(error.fields || {}).filter(([k]) => RELAYED.includes(k)));
    return {id:req.id, ok:false, error:error.message, ...fields};
  }
  return {command, failure, probe, handles:op => Object.hasOwn(SPECS, op)};
};

