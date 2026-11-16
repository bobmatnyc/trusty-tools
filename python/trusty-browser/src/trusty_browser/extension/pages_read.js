// Page reads for activation-code grants (#11239, docs/activation-codes.md), split from pages.js.
// pages.js loads this file first (importScripts in the worker; a script tag in tests). Top-level code touches
// no `chrome` API. pageInspect is injected by chrome.scripting.executeScript, so it is self-contained.
globalThis.TrustyPageRead = (() => {
  // Why: an agent addresses elements by refs, never by selectors, and must be able to reuse a ref across
  //   reads of an unchanged page (#11355).
  // What: walks the top document's light DOM and returns the read result bounded to 262144 - `reserve` UTF-8
  //   bytes. Refs live in an isolated-world registry per document: an element keeps its ref while its
  //   role-and-name signature holds, and the registry installs `resolve(ref)`, which answers the element only
  //   when it is connected and its signature still matches, else null. A ref is "<epoch>.<n>"; the random
  //   epoch keeps a ref from an earlier registry (an extension reload) from naming a new element.
  //   `options` (#11356): `scope_ref` reads one element's subtree (StaleElement when it changed),
  //   `viewport_only` keeps elements overlapping the viewport, `include_text` adds the scope's innerText, and
  //   `max_chars` bounds the encoded result. Elements fill the bound in order of distance from the viewport,
  //   then text takes what is left; any cut sets `truncated`.
  // Test: tests/test_pages_dom.py `test_read_reports_elements_and_bounds_size`,
  //   `test_refs_survive_reads_and_go_stale_when_the_element_changes`,
  //   `test_read_scopes_bounds_and_truncates_in_viewport_order`.
  function pageInspect(mode, reserve, options) {
    const LIMIT = 262144 - reserve, KEY = "__trustyBrowserRefs";
    const epoch = () => 1 + crypto.getRandomValues(new Uint32Array(1))[0] % 999999999;
    let state = globalThis[KEY];
    if (!state || state.version !== 2) {
      state = globalThis[KEY] = {version:2, epoch:epoch(), next:0, refs:new Map(), byEl:new WeakMap()};
    }
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
    const roleOf = el => el.getAttribute("role") || implicitRole(el);
    // `own` false skips the element's own text: a field's content is what an agent edits, not its identity.
    function accessibleName(el, own = true) {
      const labelled = (el.getAttribute("aria-labelledby") || "").split(/\s+/).filter(Boolean)
        .map(id => document.getElementById(id)?.innerText || "").join(" ");
      const labels = [...(el.labels || [])].map(label => label.innerText).join(" ");
      for (const value of [el.getAttribute("aria-label"), labelled, labels, el.getAttribute("placeholder"),
        own ? el.innerText : "", el.getAttribute("title"), el.getAttribute("alt")]) {
        const name = clean(value);
        if (name) return name.slice(0, 200);
      }
      return "";
    }
    const editable = el => ["input","textarea","select"].includes(el.localName) || el.isContentEditable;
    const signature = el => roleOf(el) + "\n" + accessibleName(el, !editable(el));
    function resolve(ref) {
      const entry = state.refs.get(ref), el = entry?.el.deref();
      if (el && el.isConnected && signature(el) === entry.sig) return el;
      if (entry) state.refs.delete(ref);
      return null;
    }
    function refOf(el) {
      const sig = signature(el), known = state.byEl.get(el);
      if (known && state.refs.get(known)?.sig === sig) return known;
      if (known) state.refs.delete(known);
      if (state.next > 999999) Object.assign(state, {epoch:epoch(), next:0, refs:new Map(), byEl:new WeakMap()});
      const ref = state.epoch + "." + state.next++;
      state.refs.set(ref, {el:new WeakRef(el), sig});
      state.byEl.set(el, ref);
      return ref;
    }
    state.resolve = resolve;
    for (const [ref, entry] of [...state.refs]) {
      const el = entry.el.deref();
      if (!el || !el.isConnected) state.refs.delete(ref);
    }
    function describe(el) {
      const tag = el.localName;
      const entry = {ref:refOf(el), tag, role:roleOf(el), name:accessibleName(el)};
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
      // #11285: a select lists its choices so page_select can name one.
      if (tag === "select") {
        entry.options = [...el.options].slice(0, 100).map(o => ({value:cut(o.value, 512), label:cut(clean(o.label), 200),
          ...(o.selected ? {selected:true} : {}), ...(o.disabled ? {disabled:true} : {})}));
        if (el.options.length > 100) truncated = true;
      }
      if (el.disabled === true || el.getAttribute("aria-disabled") === "true") entry.disabled = true;
      return entry;
    }
    const bytes = value => new TextEncoder().encode(JSON.stringify(value)).length;
    const shown = el => el.getClientRects().length > 0 && getComputedStyle(el).visibility === "visible";
    // Reported elements under `root` (itself included), in document order.
    function within(root) {
      const found = [...root.querySelectorAll(selector)];
      if (root.matches?.(selector)) found.unshift(root);
      return found.filter(shown);
    }
    // CSS pixels from the element's box to the viewport; 0 when they overlap.
    function distance(el) {
      const box = el.getBoundingClientRect();
      return Math.hypot(Math.max(0, box.left - innerWidth, -box.right), Math.max(0, box.top - innerHeight, -box.bottom));
    }
    // #11356: scope_ref, max_chars, viewport_only and include_text bound what a read returns.
    const scope = options.scope_ref == null ? null : resolve(options.scope_ref);
    if (options.scope_ref != null && !scope) return {error:"StaleElement"};
    const budget = Math.min(LIMIT, options.max_chars ?? LIMIT);
    const ordered = within(scope ?? document).map((el, index) => ({el, index, far:distance(el)}))
      .filter(c => !options.viewport_only || c.far === 0)
      .sort((a, b) => a.far - b.far || a.index - b.index);
    const result = {origin:location.origin, url:cut(location.href, 2048), title:cut(document.title, 200),
      text:"", elements:[], truncated:false};
    // Elements fill the budget nearest the viewport first, then return in document order.
    let used = bytes(result);
    const kept = [];
    for (const {el, index} of ordered) {
      if (kept.length === 500) { truncated = true; break; }
      const entry = describe(el), cost = bytes(entry) + 1;
      if (used + cost > budget) { truncated = true; break; }
      used += cost;
      kept.push([index, entry]);
    }
    result.elements = kept.sort((a, b) => a[0] - b[0]).map(([, entry]) => entry);
    if (options.include_text) {
      const source = scope ?? document.body;
      result.text = cut(source ? source.innerText : "", 65536);
    }
    for (let over = bytes(result) - budget; over > 0; over = bytes(result) - budget) {
      truncated = true;
      if (result.text) result.text = result.text.slice(0, Math.max(0, result.text.length - over));
      else result.elements.pop();
    }
    result.truncated = truncated;
    return result;
  }
  // In-page entry point for tests and the live check; the worker injects pageInspect itself.
  const pageRead = (reserve, options = {}) => pageInspect("read", reserve, options);
  return {pageInspect, pageRead};
})();
