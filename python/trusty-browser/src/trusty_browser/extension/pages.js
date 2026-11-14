// Page operations for activation-code grants (#11239, docs/activation-codes.md).
// The worker loads this file with importScripts; tests load it into a page. Top-level code touches no
// `chrome` API. The four `page*` functions are injected by chrome.scripting.executeScript, so each one
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

  // URL floor: https any host, or http on 127.0.0.1; no credentials; at most 8192 characters.
  function urlAllowed(url) {
    if (typeof url !== "string" || !url || url.length > 8192) return false;
    let address;
    try { address = new URL(url); } catch { return false; }
    if (address.username || address.password) return false;
    return address.protocol === "https:" || (address.protocol === "http:" && address.hostname === "127.0.0.1");
  }
  return {pageProbe, pageRead, pageClick, pageType, urlAllowed};
})();

// Why: the broker's page commands must be confined by the worker alone, whatever Chrome would allow.
// What: validates each command in the contract's check order (op, fields, email, grant, scope, tab),
//   then runs it against the explicit tab id. No path reads "the active tab".
// Test: tests/test_worker.mjs `a tab grant never reaches another tab`, `stale document refuses before the page runs`.
TrustyPages.commands = deps => {
  const {pageProbe, pageRead, pageClick, pageType, urlAllowed} = TrustyPages;
  const RELAYED = ["tab","origin","access","document"];
  const REASONS = ["revoked","tab_closed","browser_gone","unenrolled","idle","audit","closed"];
  const REF = /^[0-9]{1,9}\.[0-9]{1,6}$/;
  const tab = v => Number.isInteger(v) && v >= 0;
  const doc = v => v === null || (typeof v === "string" && v.length > 0 && v.length <= 8192);
  const ref = v => typeof v === "string" && REF.test(v);
  const SPECS = {
    page_tabs:{}, page_tab:{tab}, page_read:{tab, document:doc}, page_click:{tab, document:doc, ref},
    page_type:{tab, document:doc, ref, text:v => typeof v === "string" && v.length <= 8192},
    page_navigate:{tab, url:v => typeof v === "string" && v.length > 0 && v.length <= 8192},
    grant_end:{reason:v => REASONS.includes(v)},
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
    if (error === "StaleElement" || error === "NotEditable") throw refuse(error, {tab:req.tab});
    if (hit?.result?.ok !== true || hit.documentId !== req.document) throw refuse("OutcomeUnknown");
    return {document:req.document};
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
      await deps.endGrant(req.grant);
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
      const acting = ["page_click","page_type","page_navigate"].includes(req.op);
      return {id:req.id, ok:false, error:acting ? "OutcomeUnknown" : "BrowserDisconnected"};
    }
    const fields = Object.fromEntries(Object.entries(error.fields || {}).filter(([k]) => RELAYED.includes(k)));
    return {id:req.id, ok:false, error:error.message, ...fields};
  }
  return {command, failure, probe, handles:op => Object.hasOwn(SPECS, op)};
};

// The popup's code flow, built by the worker with its grant-table helpers (#11239).
TrustyPages.popup = deps => {
  const {bootstrap, credential, identity, changeTable, endGrant, keepListed, watchMints, watchers,
    pageOps, tabId} = deps;
  async function mint(msg, cred) {
    const {scope} = msg;
    if (!["tab","profile"].includes(scope) || (scope === "tab") !== Object.hasOwn(msg, "tab") ||
        (scope === "tab" && !tabId(msg.tab))) throw new Error("InvalidMint");
    if (scope === "tab" && !(await pageOps().probe(msg.tab)).access) throw new Error("PermissionNeeded");
    const reply = await bootstrap({op:"mint", ...cred, scope, ...(scope === "tab" ? {tab:msg.tab} : {})});
    const {code, grant, expires_at} = reply;
    if (typeof grant !== "string" || !/^g-[0-9a-f]{1,64}$/.test(grant)) throw new Error("Invalid mint reply");
    try {
      if (typeof code !== "string" || !/^tb-[0-9A-Z]{6}(-[0-9A-Z]{6}){3}$/.test(code) || reply.scope !== scope ||
          (scope === "tab" && reply.tab !== msg.tab) || !Number.isFinite(expires_at)) throw new Error("Invalid mint reply");
      for (const fresh of watchers) fresh.add(grant);
      await changeTable(t => ({...t, [grant]:{scope, tab:scope === "tab" ? msg.tab : null}}));
    } catch (error) {
      // A code the worker cannot honour is cancelled rather than left redeemable.
      try { await bootstrap({op:"revoke_grant", ...cred, grant}); } catch { /* It expires within CODE_TTL. */ }
      throw new Error(error.message === "Invalid mint reply" ? error.message : "Could not record the grant.");
    }
    chrome.alarms.create("code-expiry:" + grant, {when:expires_at * 1000 + 1000});
    return {ok:true, code, grant, scope, profile:reply.profile, ...(scope === "tab" ? {tab:msg.tab} : {}), expires_at};
  }
  // Why: the popup's code flow — preview, mint, list and revoke — with the permission settled on every exit.
  // What: routes one popup message; after any failure the table is re-settled so a permission the click
  //   requested is dropped when no grant exists.
  // Test: tests/test_worker.mjs `code preview probes the tab the popup names and never the active tab`, `failure branches never leave a grant usable or a permission held`.
  async function codeSetup(msg) {
    if (msg.type === "code_preview") {
      if (msg.tab !== null && !tabId(msg.tab)) throw new Error("InvalidRequest");
      const {enrollment} = await chrome.storage.local.get("enrollment");
      const email = await identity();
      const seen = msg.tab === null ? null : await pageOps().probe(msg.tab);
      return {ok:true, paired:!!enrollment && enrollment.email === email, email, profile:enrollment?.profile ?? null,
        target:seen && {tab:msg.tab, access:seen.access, origin:seen.origin, title:seen.title}};
    }
    try {
      const cred = await credential();
      if (msg.type === "mint") return await mint(msg, cred);
      if (msg.type === "grants") {
        const fresh = watchMints();
        let reply;
        try { reply = await bootstrap({op:"grants", ...cred}); } catch (error) { watchers.delete(fresh); throw error; }
        if (!Array.isArray(reply.grants)) { watchers.delete(fresh); throw new Error("Invalid grants reply"); }
        await keepListed(new Set(reply.grants.map(entry => entry?.grant)), fresh);
        const grants = await Promise.all(reply.grants.map(async entry => {
          if (entry?.scope !== "tab" || !tabId(entry.tab)) return entry;
          const seen = await pageOps().probe(entry.tab);
          return {...entry, origin:seen.origin, title:seen.title};
        }));
        return {ok:true, grants};
      }
      if (msg.type === "revoke_grant") {
        if (typeof msg.grant !== "string" || !msg.grant) throw new Error("InvalidRequest");
        try {
          const {revoked} = await bootstrap({op:"revoke_grant", ...cred, grant:msg.grant});
          await endGrant(msg.grant);
          return {ok:true, revoked};
        } catch (error) {
          if (error.response?.error === "UnknownGrant") await endGrant(msg.grant);
          throw error;
        }
      }
      const {revoked} = await bootstrap({op:"revoke_grants", ...cred});
      await changeTable(() => ({}));
      return {ok:true, revoked};
    } catch (error) {
      await changeTable(t => t).catch(() => {});
      throw error;
    }
  }
  return codeSetup;
};
