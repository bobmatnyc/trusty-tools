// Page reads for activation-code grants (#11239, docs/activation-codes.md), split from pages.js.
// pages.js loads this file first (importScripts in the worker; a script tag in tests). Top-level code touches
// no `chrome` API. pageInspect is injected by chrome.scripting.executeScript, so it is self-contained.
globalThis.TrustyPageRead = (() => {
  // Why: an agent addresses elements by refs, never by selectors, and must be able to reuse a ref across
  //   reads of an unchanged page (#11355).
  // What: walks the top document, its open shadow roots and same-origin frames (#11290), and returns the read result bounded to 262144 - `reserve` UTF-8
  //   bytes. Refs live in an isolated-world registry per document: an element keeps its ref while its
  //   role-and-name signature holds, and the registry installs `resolve(ref)`, which answers the element only
  //   when it is connected and its signature still matches, else null. A ref is "<epoch>.<n>"; the random
  //   epoch keeps a ref from an earlier registry (an extension reload) from naming a new element.
  //   `options` (#11356): `scope_ref` reads one element's subtree (StaleElement when it changed),
  //   `viewport_only` keeps elements overlapping the viewport, `include_text` adds the scope's innerText, and
  //   `max_chars` bounds the encoded result. With `include_text` the text keeps up to half the bound (#11284);
  //   elements fill the rest in order of distance from the viewport; any cut sets `truncated`. An entry outside the top light DOM carries
  //   `within`, its hop path ("iframe/shadow"); viewport distance is measured in top-document coordinates.
  // Test: tests/test_pages_dom.py `test_read_reports_elements_and_bounds_size`,
  //   `test_refs_survive_reads_and_go_stale_when_the_element_changes`,
  //   `test_read_scopes_bounds_and_truncates_in_viewport_order`,
  //   `test_read_and_act_inside_iframes_and_shadow_roots`, `test_scope_ref_and_find_cross_shadow_roots`,
  //   `test_iframe_reads_measure_distance_in_top_coordinates`,
  //   `test_drag_reaches_a_draggable_inside_an_open_shadow_root` (#11288); tests/extension/test_frames.mjs
  //   `include_text keeps a text share on an element-heavy page` (#11284). Mode "find" is page_find (see find below).
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
        .map(role => "[role=" + role + "]").join(",") +
      ",[draggable=true],[aria-dropeffect]:not([aria-dropeffect=none]),[dropzone]";
    // #11288: a drop zone is often a bare div. When the page offers something to drag, elements whose
    // id or class names a drop target are reported too, so page_drag has a target ref.
    const DROP_NAMED = "[id*=drop i],[class*=drop i],[data-drop],[data-dropzone]";
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
        .map(id => el.getRootNode().getElementById?.(id)?.innerText || "").join(" ");
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
    // #11290: the hop path from the top document's light DOM to `el` ("", "shadow", "iframe/shadow"), or null
    // when `el` is not connected to the top document through every shadow host and frame element above it.
    function hops(el) {
      const path = [];
      for (let node = el; node.isConnected;) {
        const root = node.getRootNode();
        if (root === document) return path.join("/");
        const frame = root.host ? null : root.defaultView?.frameElement;
        if (!root.host && !frame) return null;
        path.unshift(root.host ? "shadow" : "iframe");
        node = root.host || frame;
      }
      return null;
    }
    function resolve(ref) {
      const entry = state.refs.get(ref), el = entry?.el.deref();
      if (el && hops(el) !== null && signature(el) === entry.sig) return el;
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
      if (!el || hops(el) === null) state.refs.delete(ref);
    }
    function describe(el) {
      const tag = el.localName;
      const entry = {ref:refOf(el), tag, role:roleOf(el), name:accessibleName(el)}, path = hops(el);
      if (path) entry.within = path;
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
      if (el.getAttribute("draggable") === "true") entry.draggable = true;
      return entry;
    }
    const bytes = value => new TextEncoder().encode(JSON.stringify(value)).length;
    // #11357: the container kinds a match reports, nearest ancestor first.
    const CONTAINERS = [["list","ul,ol,[role=list]"], ["row","tr,[role=row]"],
      ["dialog","dialog,[role=dialog],[role=alertdialog]"], ["form","form,[role=form]"],
      ["navigation","nav,[role=navigation]"], ["main","main,[role=main]"]];
    const ANY_CONTAINER = CONTAINERS.map(([, css]) => css).join(",");
    // A container's name: its label, else a row's own text, else its first heading, legend or caption.
    function containerName(box, kind) {
      const labelled = (box.getAttribute("aria-labelledby") || "").split(/\s+/).filter(Boolean)
        .map(id => box.getRootNode().getElementById?.(id)?.innerText || "").join(" ");
      const heading = kind === "row" ? box : box.querySelector("h1,h2,h3,h4,h5,h6,legend,caption,[role=heading]");
      for (const value of [box.getAttribute("aria-label"), labelled, heading?.innerText]) {
        const name = clean(value);
        if (name) return name.slice(0, 200);
      }
      return "";
    }
    function container(el) {
      const box = el.parentElement?.closest(ANY_CONTAINER);
      if (!box) return null;
      const [kind] = CONTAINERS.find(([, css]) => box.matches(css));
      return {role:kind, name:containerName(box, kind), ref:refOf(box)};
    }
    // Why: an agent needs one element (the Delete button in the row for X), not the whole page (#11357).
    // What: matches the reported elements under `within_ref` (descendants only; StaleElement when it does not
    //   resolve) by exact `role`, and case-insensitive substrings of `name` and of `text` (the element's own
    //   text or its container's). Returns at most `limit` matches in document order, each with `in`, its nearest
    //   container, and `truncated` when more matched.
    function find() {
      const fold = value => clean(value).toLowerCase();
      const scope = options.within_ref == null ? null : resolve(options.within_ref);
      if (options.within_ref != null && !scope) return {error:"StaleElement"};
      const result = {origin:location.origin, matches:[], truncated:false};
      for (const el of within(scope ?? document)) {
        if (el === scope) continue;
        if (options.role != null && roleOf(el) !== options.role) continue;
        if (options.name != null && !fold(accessibleName(el)).includes(fold(options.name))) continue;
        const box = el.parentElement?.closest(ANY_CONTAINER);
        if (options.text != null && ![el, box].some(n => n && fold(n.innerText).includes(fold(options.text)))) continue;
        if (result.matches.length === options.limit) { result.truncated = true; break; }
        result.matches.push({...describe(el), in:container(el)});
      }
      while (bytes(result) > LIMIT) { result.truncated = true; result.matches.pop(); }
      return result;
    }
    const shown = el => el.getClientRects().length > 0 &&
      el.ownerDocument.defaultView.getComputedStyle(el).visibility === "visible";
    // #11290: `root` (itself included) and everything under it in document order, descending into each open
    // shadow root after its host and each shown same-origin frame. A closed shadow root is invisible to page
    // script and a cross-origin frame's contentDocument is null, so both are skipped.
    function* walk(root) {
      for (const el of root.nodeType === 1 ? [root, ...root.querySelectorAll("*")] : root.querySelectorAll("*")) {
        yield el;
        if (el.shadowRoot) yield* walk(el.shadowRoot);
        if (el.localName !== "iframe" && el.localName !== "frame" || !shown(el)) continue;
        let inner = null;
        try { inner = el.contentDocument; } catch { inner = null; }
        if (inner) yield* walk(inner);
      }
    }
    // #11288: drop-named elements count once any walked node, light DOM, open shadow root or same-origin
    // frame, is draggable; the check covers the whole document, so a scoped read sees the same set.
    let reported = null;
    const reports = el => {
      reported ??= [...walk(document)].some(node => node.matches("[draggable=true]")) ?
        selector + "," + DROP_NAMED : selector;
      return el.matches(reported);
    };
    // Reported elements under `root` (itself included), in document order.
    const within = root => [...walk(root)].filter(el => reports(el) && shown(el));
    // CSS pixels from the element's box to the top viewport; 0 when they overlap. #11290: a box inside a
    // same-origin frame is shifted by each frame element's content-box offset up the chain.
    function distance(el) {
      let {left, right, top, bottom} = el.getBoundingClientRect();
      for (let frame = el.ownerDocument.defaultView?.frameElement; frame && el.ownerDocument !== document;) {
        const at = frame.getBoundingClientRect(), css = frame.ownerDocument.defaultView.getComputedStyle(frame);
        const dx = at.left + frame.clientLeft + parseFloat(css.paddingLeft);
        const dy = at.top + frame.clientTop + parseFloat(css.paddingTop);
        [left, right, top, bottom] = [left + dx, right + dx, top + dy, bottom + dy];
        el = frame;
        frame = frame.ownerDocument.defaultView?.frameElement;
      }
      return Math.hypot(Math.max(0, left - innerWidth, -right), Math.max(0, top - innerHeight, -bottom));
    }
    if (mode === "find") return find();
    // #11356: scope_ref, max_chars, viewport_only and include_text bound what a read returns.
    const scope = options.scope_ref == null ? null : resolve(options.scope_ref);
    if (options.scope_ref != null && !scope) return {error:"StaleElement"};
    const budget = Math.min(LIMIT, options.max_chars ?? LIMIT);
    // #11301: a cross-origin child frame's own viewport says nothing about the tab's, so its entries carry no
    // distance: they keep document order and viewport_only does not filter them.
    const far = options.frame === true ? () => 0 : distance;
    const ordered = within(scope ?? document).map((el, index) => ({el, index, far:far(el)}))
      .filter(c => !options.viewport_only || c.far === 0)
      .sort((a, b) => a.far - b.far || a.index - b.index);
    const result = {origin:location.origin, url:cut(location.href, 2048), title:cut(document.title, 200),
      text:"", elements:[], truncated:false};
    // #11284: with include_text, the text keeps up to half the budget before elements fill the rest.
    let text = "";
    if (options.include_text) {
      const source = scope ?? document.body;
      text = cut(source ? source.innerText : "", 65536);
    }
    const share = Math.min(Math.floor(budget / 2), bytes(text) - 2);
    // Elements fill the budget nearest the viewport first, then return in document order.
    let used = bytes(result);
    const kept = [];
    for (const {el, index} of ordered) {
      if (kept.length === 500) { truncated = true; break; }
      const entry = describe(el), cost = bytes(entry) + 1;
      if (used + cost > budget - share) { truncated = true; break; }
      used += cost;
      kept.push([index, entry]);
    }
    result.elements = kept.sort((a, b) => a[0] - b[0]).map(([, entry]) => entry);
    result.text = text;
    for (let over = bytes(result) - budget; over > 0; over = bytes(result) - budget) {
      truncated = true;
      // #11356: text goes first, then elements, then the title and url, so the loop always ends.
      if (!result.text && result.elements.length) { result.elements.pop(); continue; }
      const key = ["text", "title", "url"].find(k => result[k]);
      if (!key) break;
      result[key] = result[key].slice(0, Math.max(0, result[key].length - over));
    }
    result.truncated = truncated;
    return result;
  }
  // Why: the worker learns a tab's cross-origin child frames without the webNavigation permission (#11301).
  // What: injected into every frame (allFrames). `root` is true for the top frame and for a frame whose parent
  //   is cross-origin; only a root's read is reported, since a root's walk already covers its same-origin
  //   descendants. A root also lists `children`: the declared origins (from `src`) of the shown cross-origin
  //   http(s) frames its walk reaches, at most 50, for the denied list. Reads no page content.
  // Test: tests/extension/test_frames.mjs `a granted child frame is read with attribution`.
  function pageFrames() {
    let root = window === window.top;
    if (!root) {
      try { root = window.parent.location.origin !== location.origin; } catch { root = true; }
    }
    const children = new Set();
    const shown = el => el.getClientRects().length > 0 &&
      el.ownerDocument.defaultView.getComputedStyle(el).visibility === "visible";
    function scan(node) {
      for (const el of node.querySelectorAll("*")) {
        if (el.shadowRoot) scan(el.shadowRoot);
        if (el.localName !== "iframe" && el.localName !== "frame" || !shown(el)) continue;
        let inner = null;
        try { inner = el.contentDocument; } catch { inner = null; }
        if (inner) { scan(inner); continue; }
        try {
          const url = new URL(el.getAttribute("src") || "about:blank", el.ownerDocument.baseURI);
          if (url.protocol === "https:" || url.protocol === "http:") children.add(url.origin);
        } catch { /* An unparsable src names no origin. */ }
      }
    }
    if (root) scan(document);
    return {origin:location.origin, url:location.href.slice(0, 2048), root, children:[...children].slice(0, 50)};
  }
  // In-page entry point for tests and the live check; the worker injects pageInspect itself.
  const pageRead = (reserve, options = {}) => pageInspect("read", reserve, options);
  const pageFind = options => pageInspect("find", 512, {role:null, name:null, text:null, within_ref:null, limit:10,
    ...options});
  return {pageInspect, pageRead, pageFind, pageFrames};
})();
