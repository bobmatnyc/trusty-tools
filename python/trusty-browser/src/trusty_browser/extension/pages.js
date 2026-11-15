// Page operations for activation-code grants (#11239, docs/activation-codes.md).
// The worker loads this file with importScripts; tests load it into a page. Top-level code touches no
// `chrome` API. The `page*` function declarations are injected by chrome.scripting.executeScript, so each one
// is self-contained: Chrome serializes the function source and runs it in the tab's isolated world.
globalThis.TrustyPages = (() => {
  // Returns [origin, title] and changes nothing; the InjectionResult supplies the documentId.
  function pageProbe() { return [location.origin, document.title]; }

  // Why: an agent addresses elements by refs from its own latest read, never by selectors.
  // What: walks the top document, its open shadow roots and same-origin iframes (#11290), keeps ref -> WeakRef(element) in an isolated-world map that
  //   replaces any earlier map, and returns the read result bounded to 262144 UTF-8 bytes.
  // Test: tests/test_pages_dom.py `test_read_reports_elements_and_bounds_size`,
  //   `test_read_and_act_inside_iframes_and_shadow_roots`.
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
        .map(id => el.getRootNode().getElementById?.(id)?.innerText || "").join(" ");
      const labels = [...(el.labels || [])].map(label => label.innerText).join(" ");
      for (const value of [el.getAttribute("aria-label"), labelled, labels, el.getAttribute("placeholder"),
        el.innerText, el.getAttribute("title"), el.getAttribute("alt")]) {
        const name = clean(value);
        if (name) return name.slice(0, 200);
      }
      return "";
    }
    const shown = el => el.getClientRects().length > 0 &&
      el.ownerDocument.defaultView.getComputedStyle(el).visibility === "visible";
    // #11290: document order through open shadow roots and same-origin iframes. A closed shadow root
    // is invisible to page script; a cross-origin frame's contentDocument is null, so both are skipped.
    // Each hit carries its hop path ("iframe", "shadow", "iframe/shadow", ...); "" is the top light DOM.
    function* walk(root, within) {
      for (const el of root.querySelectorAll("*")) {
        if (el.matches(selector)) yield [el, within];
        const hop = within ? within + "/" : "";
        if (el.shadowRoot) yield* walk(el.shadowRoot, hop + "shadow");
        if ((el.localName === "iframe" || el.localName === "frame") && shown(el)) {
          let inner = null;
          try { inner = el.contentDocument; } catch { inner = null; }
          if (inner) yield* walk(inner, hop + "iframe");
        }
      }
    }
    const elements = [];
    for (const [el, within] of walk(document, "")) {
      if (!shown(el)) continue;
      if (elements.length === 500) { truncated = true; break; }
      const ref = read + "." + elements.length, tag = el.localName;
      const entry = {ref, tag, role:el.getAttribute("role") || implicitRole(el), name:accessibleName(el)};
      if (within) entry.within = within;
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
    // #11290: connected all the way up to this document, through shadow hosts and frame elements.
    let up = el;
    while (up && up.isConnected && up.getRootNode() !== document) {
      const root = up.getRootNode();
      up = root.host || root.defaultView?.frameElement || null;
    }
    if (!up || !up.isConnected) return {error:"StaleElement"};
    el.scrollIntoView({block:"center", inline:"center"});
    const box = el.getBoundingClientRect();
    const at = {bubbles:true, cancelable:true, composed:true, view:el.ownerDocument.defaultView,
      clientX:box.left + box.width / 2, clientY:box.top + box.height / 2};
    el.dispatchEvent(new PointerEvent("pointerdown", at));
    el.dispatchEvent(new MouseEvent("mousedown", at));
    el.dispatchEvent(new PointerEvent("pointerup", at));
    el.dispatchEvent(new MouseEvent("mouseup", at));
    if (typeof el.focus === "function" && el.tabIndex >= 0) el.focus();
    el.click();
    return {ok:true};
  }

  // Why: page_type and page_press must reach the page as keystrokes: handlers listen for keydown, and a
  //   form submits on Enter (#11286). Synthetic key events carry no default action, so this emulates it.
  // What: action "type" checks that `ref` is a supported editable target (else NotEditable), focuses it,
  //   selects its content and presses each character of `input` ("\n" or "\r" is Enter, "\t" is Tab).
  //   Action "press" focuses `ref` when given (null keeps the focused element) and presses
  //   `input.key` with `input.modifiers`. Each press fires modifier keydowns, keydown, keypress (printable
  //   keys and Enter without Control or Meta), the default action unless a handler cancelled, keyup, then
  //   modifier keyups. Self-contained: Chrome serializes this one function into the page.
  // Test: tests/test_pages_dom.py `test_type_emits_key_events_and_enter_submits_the_form`,
  //   `test_press_tab_and_shift_tab_follow_the_focus_order`,
  //   `test_press_edits_fields_and_honours_chords_and_prevent_default`,
  //   `test_type_sets_values_and_refuses_non_editable`.
  function pageKeys(ref, action, input) {
    let el = null;
    if (ref !== null) {
      const state = globalThis.__trustyBrowserRefs, [read] = ref.split(".");
      el = state && Number(read) === state.read ? state.map.get(ref)?.deref() : null;
      if (!el || !el.isConnected) return {error:"StaleElement"};
      {
        // #11290: connected all the way up to this document, through shadow hosts and frame elements.
        let up = el;
        while (up && up.isConnected && up.getRootNode() !== document) {
          const root = up.getRootNode();
          up = root.host || root.defaultView?.frameElement || null;
        }
        if (!up || !up.isConnected) return {error:"StaleElement"};
      }
    }
    const KINDS = ["text","search","email","url","tel","password","number"];
    const typeOf = node => (node.getAttribute("type") || "text").toLowerCase();
    const isField = node => node?.localName === "textarea" || (node?.localName === "input" && KINDS.includes(typeOf(node)));
    const writable = node => (isField(node) || node?.isContentEditable === true) && !node.disabled && !node.readOnly;
    if (action === "type" && !writable(el)) return {error:"NotEditable"};
    const NAMED = {Enter:["Enter",13], Tab:["Tab",9], Escape:["Escape",27], Backspace:["Backspace",8],
      Delete:["Delete",46], ArrowUp:["ArrowUp",38], ArrowDown:["ArrowDown",40], ArrowLeft:["ArrowLeft",37],
      ArrowRight:["ArrowRight",39], Home:["Home",36], End:["End",35], PageUp:["PageUp",33], PageDown:["PageDown",34]};
    const MODS = {Control:["ControlLeft",17,"ctrlKey"], Alt:["AltLeft",18,"altKey"],
      Shift:["ShiftLeft",16,"shiftKey"], Meta:["MetaLeft",91,"metaKey"]};
    const raw = new Map(), dirty = new Set();
    let whole = null;
    const describe = (key, shift) => {
      if (key === "Space" || key === " ") return {key:" ", code:"Space", keyCode:32, text:" "};
      if (NAMED[key]) return {key, code:NAMED[key][0], keyCode:NAMED[key][1]};
      const text = shift && /^[a-z]$/.test(key) ? key.toUpperCase() : key, upper = text.toUpperCase();
      if (/^[A-Z]$/.test(upper)) return {key:text, code:"Key" + upper, keyCode:upper.charCodeAt(0), text};
      if (/^[0-9]$/.test(text)) return {key:text, code:"Digit" + text, keyCode:text.charCodeAt(0), text};
      return {key:text, code:"", keyCode:0, text};
    };
    const fire = (node, type, d, flags) => node.dispatchEvent(new KeyboardEvent(type, {...flags, key:d.key, code:d.code,
      keyCode:type === "keypress" ? (d.text ?? "\r").codePointAt(0) : d.keyCode, which:d.keyCode,
      charCode:type === "keypress" ? (d.text ?? "\r").codePointAt(0) : 0,
      bubbles:true, cancelable:true, composed:true, view:window}));
    // The key's next target: the element focus moved to during `step`, else the current one.
    const follow = (node, step) => {
      const before = document.activeElement, result = step();
      const now = document.activeElement;
      if (now !== before) node = now && now !== document.body ? now : document.body;
      else if (!node.isConnected) node = document.body;
      return [node, result];
    };
    const selectable = node => { try { return node.selectionStart !== null; } catch { return false; } };
    // email and number inputs have no selection API and sanitize partial text, so keep the typed text.
    const current = node => selectable(node) ? node.value : raw.get(node) ?? node.value;
    const span = node => {
      if (whole === node) return [0, current(node).length];
      if (selectable(node)) return [node.selectionStart, node.selectionEnd];
      return [current(node).length, current(node).length];
    };
    // Replaces [from, to) of a field's value through the native setter, so framework value trackers see it.
    function edit(node, from, to, data, inputType) {
      const value = current(node), next = value.slice(0, from) + data + value.slice(to);
      if (data && node.maxLength >= 0 && next.length > node.maxLength) return;
      if (!node.dispatchEvent(new InputEvent("beforeinput", {bubbles:true, cancelable:true, composed:true,
        inputType, data:data || null}))) return;
      const proto = node.localName === "textarea" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, "value").set.call(node, next);
      raw.set(node, next);
      whole = null;
      try { node.setSelectionRange(from + data.length, from + data.length); } catch { /* email, number */ }
      node.dispatchEvent(new InputEvent("input", {bubbles:true, composed:true, inputType, data:data || null}));
      dirty.add(node);
    }
    function insert(node, data) {
      if (!writable(node)) return;
      if (node.isContentEditable) { document.execCommand("insertText", false, data); return; }
      const [from, to] = span(node);
      edit(node, from, to, data, "insertText");
    }
    function remove(node, backward) {
      if (!writable(node)) return;
      if (node.isContentEditable) { document.execCommand(backward ? "delete" : "forwardDelete"); return; }
      let [from, to] = span(node);
      if (from === to) { if (backward) from = Math.max(0, from - 1); else to = Math.min(current(node).length, to + 1); }
      if (from !== to) edit(node, from, to, "", backward ? "deleteContentBackward" : "deleteContentForward");
    }
    function commit(node) {
      if (dirty.delete(node)) node.dispatchEvent(new Event("change", {bubbles:true}));
    }
    function caret(node, key, extend) {
      if (node.isContentEditable) {
        const [direction, unit] = {ArrowLeft:["backward","character"], ArrowRight:["forward","character"],
          ArrowUp:["backward","line"], ArrowDown:["forward","line"], Home:["backward","lineboundary"],
          End:["forward","lineboundary"], PageUp:["backward","paragraphboundary"],
          PageDown:["forward","paragraphboundary"]}[key];
        getSelection().modify(extend ? "extend" : "move", direction, unit);
        return;
      }
      let start, end;
      try { start = node.selectionStart; end = node.selectionEnd; } catch { return; }
      if (start === null) return;
      const value = node.value, back = node.selectionDirection === "backward";
      const head = extend ? (back ? start : end) : (["ArrowLeft","ArrowUp","Home","PageUp"].includes(key) ? start : end);
      const lineStart = value.lastIndexOf("\n", head - 1) + 1, lineEnd = (value.indexOf("\n", head) + 1 || value.length + 1) - 1;
      let to = head;
      if (key === "ArrowLeft") to = extend || start === end ? Math.max(0, head - 1) : start;
      else if (key === "ArrowRight") to = extend || start === end ? Math.min(value.length, head + 1) : end;
      else if (key === "Home") to = lineStart;
      else if (key === "End") to = lineEnd;
      else if (key === "PageUp" || (key === "ArrowUp" && lineStart === 0)) to = 0;
      else if (key === "PageDown" || (key === "ArrowDown" && lineEnd === value.length)) to = value.length;
      else if (key === "ArrowUp") {
        const above = lineStart >= 2 ? value.lastIndexOf("\n", lineStart - 2) + 1 : 0;
        to = Math.min(above + head - lineStart, lineStart - 1);
      } else {
        const below = lineEnd + 1, belowEnd = (value.indexOf("\n", below) + 1 || value.length + 1) - 1;
        to = Math.min(below + head - lineStart, belowEnd);
      }
      const anchor = extend ? (back ? end : start) : to;
      node.setSelectionRange(Math.min(anchor, to), Math.max(anchor, to), to < anchor ? "backward" : "forward");
    }
    // Sequential focus order: positive tabindex ascending, then tabindex 0 in document order.
    // An editing host is tabbable though Chromium reports tabIndex -1 for it without a tabindex attribute.
    const tabIndexOf = n => n.hasAttribute("tabindex") || !n.isContentEditable || n.parentElement?.isContentEditable
      ? n.tabIndex : 0;
    function moveFocus(from, step) {
      const all = [...document.querySelectorAll("a[href],area[href],button,input,select,textarea,iframe,summary," +
        "[tabindex],[contenteditable]")].filter(n => tabIndexOf(n) >= 0 && !n.matches(":disabled") && !n.closest("[inert]") &&
        !(n.localName === "input" && typeOf(n) === "hidden") && n.getClientRects().length > 0 &&
        getComputedStyle(n).visibility === "visible" && !(n.localName === "input" && typeOf(n) === "radio" &&
        n.name && !n.checked && [...document.getElementsByName(n.name)].some(r => r !== n && r.form === n.form &&
        typeOf(r) === "radio" && (r.checked || r.compareDocumentPosition(n) & Node.DOCUMENT_POSITION_FOLLOWING))));
      const order = [...all.filter(n => tabIndexOf(n) > 0).sort((a, b) => tabIndexOf(a) - tabIndexOf(b)),
        ...all.filter(n => tabIndexOf(n) === 0)];
      if (!order.length) return;
      let at = order.indexOf(from), next;
      if (at >= 0) next = order[(at + step + order.length) % order.length];
      else if (from === document.body) next = step > 0 ? order[0] : order.at(-1);
      else {
        const after = order.filter(n => from.compareDocumentPosition(n) & Node.DOCUMENT_POSITION_FOLLOWING);
        next = step > 0 ? after[0] ?? order[0] : order.filter(n => !after.includes(n)).at(-1) ?? order.at(-1);
      }
      commit(from);
      next.focus();
      if (isField(next)) try { next.setSelectionRange(0, next.value.length); } catch { /* email, number */ }
    }
    function submitFrom(node) {
      const form = node.form;
      if (!form) return;
      const controls = [...form.elements];
      const button = controls.find(n => (n.localName === "button" && n.type === "submit") ||
        (n.localName === "input" && ["submit","image"].includes(typeOf(n))));
      if (button) { if (!button.disabled) button.click(); return; }
      const blocking = ["text","search","email","url","tel","password","number","date","month","week","time","datetime-local"];
      if (controls.filter(n => n.localName === "input" && blocking.includes(typeOf(n))).length <= 1) form.requestSubmit();
    }
    // The browser's default action for one unprevented key; returns work that runs after keyup, if any.
    function defaultAction(node, d, flags) {
      const activates = node.localName === "button" || node.localName === "summary" || (node.localName === "input" &&
        ["submit","button","reset","image","checkbox","radio"].includes(typeOf(node)));
      if ((flags.ctrlKey || flags.metaKey) && d.key.toLowerCase() === "a") {
        if (isField(node)) { whole = node; try { node.setSelectionRange(0, node.value.length); } catch { /* email, number */ } }
        else getSelection().selectAllChildren(node.isContentEditable ? node : document.body);
      } else if (d.text !== undefined && !flags.ctrlKey && !flags.metaKey) {
        if (writable(node)) insert(node, d.text);
        else if (d.text === " " && activates) return () => node.click();
        else if (d.text === " ") document.scrollingElement?.scrollBy(0, (flags.shiftKey ? -0.875 : 0.875) * innerHeight);
      } else if (d.key === "Enter") {
        if (node.localName === "textarea" && writable(node)) insert(node, "\n");
        else if (node.isContentEditable) document.execCommand(flags.shiftKey ? "insertLineBreak" : "insertParagraph");
        else if (node.localName === "input" && !activates) { commit(node); submitFrom(node); }
        else if (activates && !["checkbox","radio"].includes(typeOf(node)) || (node.localName === "a" && node.href)) node.click();
        else if (["checkbox","radio"].includes(typeOf(node))) submitFrom(node);
      } else if (d.key === "Tab") moveFocus(node, flags.shiftKey ? -1 : 1);
      else if (d.key === "Backspace" || d.key === "Delete") remove(node, d.key === "Backspace");
      else if (d.key.startsWith("Arrow") || ["Home","End","PageUp","PageDown"].includes(d.key)) {
        if (isField(node) || node.isContentEditable) caret(node, d.key, flags.shiftKey);
        else {
          const page = 0.875 * innerHeight, s = document.scrollingElement;
          const [x, y] = {ArrowUp:[0,-40], ArrowDown:[0,40], ArrowLeft:[-40,0], ArrowRight:[40,0],
            PageUp:[0,-page], PageDown:[0,page]}[d.key] || [0, 0];
          if (d.key === "Home") s?.scrollTo(s.scrollLeft, 0);
          else if (d.key === "End") s?.scrollTo(s.scrollLeft, s.scrollHeight);
          else s?.scrollBy(x, y);
        }
      }
      return null;
    }
    function pressOne(key, modifiers, start) {
      const flags = {ctrlKey:false, altKey:false, shiftKey:false, metaKey:false};
      const held = ["Control","Alt","Shift","Meta"].filter(m => modifiers.includes(m));
      let node = start, go, after = null;
      for (const m of held) {
        flags[MODS[m][2]] = true;
        [node] = follow(node, () => fire(node, "keydown", {key:m, code:MODS[m][0], keyCode:MODS[m][1]}, flags));
      }
      const d = describe(key, flags.shiftKey);
      [node, go] = follow(node, () => fire(node, "keydown", d, flags));
      if (go && (d.text !== undefined || d.key === "Enter") && !flags.ctrlKey && !flags.metaKey) {
        [node, go] = follow(node, () => fire(node, "keypress", d, flags));
      }
      if (go) [node, after] = follow(node, () => defaultAction(node, d, flags));
      [node] = follow(node, () => fire(node, "keyup", d, flags));
      if (after) [node] = follow(node, after);
      for (const m of held.reverse()) {
        flags[MODS[m][2]] = false;
        [node] = follow(node, () => fire(node, "keyup", {key:m, code:MODS[m][0], keyCode:MODS[m][1]}, flags));
      }
      return node;
    }
    if (el && typeof el.focus === "function") el.focus();
    const active = document.activeElement;
    let node = el ?? (active && active !== document.body ? active : document.body);
    if (action === "press") {
      pressOne(input.key, input.modifiers, node);
      return {ok:true};
    }
    // "type": the text replaces the element's content, as one keystroke per character.
    if (el.isContentEditable) {
      const range = document.createRange();
      range.selectNodeContents(el);
      getSelection().removeAllRanges();
      getSelection().addRange(range);
      if (!input) document.execCommand("delete");
    } else {
      whole = el;
      if (!input) edit(el, 0, el.value.length, "", "deleteContentBackward");
    }
    const chars = [...input];
    for (let i = 0; i < chars.length; i++) {
      const ch = chars[i];
      if (ch === "\r" && chars[i + 1] === "\n") continue;
      const key = ch === "\n" || ch === "\r" ? "Enter" : ch === "\t" ? "Tab" : ch;
      node = pressOne(key, /^[A-Z]$/.test(ch) ? ["Shift"] : [], node);
    }
    for (const touched of [...dirty]) commit(touched);
    return {ok:true};
  }

  // In-page entry points for tests and the live check; the worker injects pageKeys itself.
  const pageType = (ref, text) => pageKeys(ref, "type", text);
  const pagePress = (ref, key, modifiers) => pageKeys(ref, "press", {key, modifiers});
  // Why: a native <select> ignores synthetic clicks on its options and is not a typing target, so an agent
  //   had no way to choose a value (#11285).
  // What: resolves the ref; a target that is not a single-choice, enabled select answers NotSelectable. Picks
  //   the first enabled option whose value equals `value`, else whose label does; none answers NoSuchOption.
  //   An option already selected dispatches nothing. Otherwise sets selectedIndex and dispatches bubbling
  //   input and change. `changed` compares the value before and after, so a handler that reverts the choice
  //   reports changed:false; `value` is the select's value afterwards.
  // Test: tests/test_pages_dom.py `test_select_sets_value_and_reports_no_effect`,
  //   `test_select_refuses_non_select_and_unknown_options`, `test_select_fixture_logs_the_change`.
  function pageSelect(ref, value) {
    const state = globalThis.__trustyBrowserRefs, [read] = ref.split(".");
    const el = state && Number(read) === state.read ? state.map.get(ref)?.deref() : null;
    if (!el || !el.isConnected) return {error:"StaleElement"};
    {
      // #11290: connected all the way up to this document, through shadow hosts and frame elements.
      let up = el;
      while (up && up.isConnected && up.getRootNode() !== document) {
        const root = up.getRootNode();
        up = root.host || root.defaultView?.frameElement || null;
      }
      if (!up || !up.isConnected) return {error:"StaleElement"};
    }
    if (el.localName !== "select" || el.multiple || el.disabled || el.matches(":disabled")) return {error:"NotSelectable"};
    const options = [...el.options].filter(o => !o.disabled && !o.matches(":disabled"));
    const option = options.find(o => o.value === value) || options.find(o => o.label.trim() === value.trim());
    if (!option) return {error:"NoSuchOption"};
    const before = el.value;
    if (option.selected) return {ok:true, changed:false, value:before};
    el.focus();
    el.selectedIndex = option.index;
    el.dispatchEvent(new Event("input", {bubbles:true, composed:true}));
    el.dispatchEvent(new Event("change", {bubbles:true}));
    return {ok:true, changed:el.value !== before, value:el.value};
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
      {
        // #11290: connected all the way up to this document, through shadow hosts and frame elements.
        let up = el;
        while (up && up.isConnected && up.getRootNode() !== document) {
          const root = up.getRootNode();
          up = root.host || root.defaultView?.frameElement || null;
        }
        if (!up || !up.isConnected) return {error:"StaleElement"};
      }
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

  // Why: menus and tooltips open on pointer entry, and an agent had no way to move the pointer (#11287).
  // What: resolves the ref, scrolls it into view and moves the synthetic pointer from the element this
  //   document last hovered to it: out/leave on the old path, over/enter on the new one (enter from the
  //   outermost ancestor down, leave from the innermost up), then pointermove/mousemove at the centre.
  //   Hovering the same element again sends only the moves. The events are untrusted, so CSS `:hover` does
  //   not match; that needs trusted input (#11294).
  // Test: tests/test_pages_dom.py `test_hover_fires_enter_over_and_move_then_leaves_the_old_target`,
  //   `test_hover_fixture_reveals_the_hover_action`.
  function pageHover(ref) {
    const state = globalThis.__trustyBrowserRefs, [read] = ref.split(".");
    const el = state && Number(read) === state.read ? state.map.get(ref)?.deref() : null;
    if (!el || !el.isConnected) return {error:"StaleElement"};
    el.scrollIntoView({block:"center", inline:"center"});
    const box = el.getBoundingClientRect();
    const at = {cancelable:true, composed:true, view:window, pointerType:"mouse", isPrimary:true,
      clientX:box.left + box.width / 2, clientY:box.top + box.height / 2};
    const KEY = "__trustyBrowserHover";
    let last = globalThis[KEY]?.deref();
    if (!last || !last.isConnected || last.ownerDocument !== document) last = null;
    const path = node => { const out = []; for (; node; node = node.parentElement) out.push(node); return out; };
    const fire = (target, kinds, bubbles, relatedTarget) => {
      for (const [Kind, type] of kinds) target.dispatchEvent(new Kind(type, {...at, bubbles, relatedTarget}));
    };
    if (last !== el) {
      const old = last ? path(last) : [], now = path(el);
      if (last) {
        fire(last, [[PointerEvent, "pointerout"], [MouseEvent, "mouseout"]], true, el);
        for (const node of old.filter(n => !now.includes(n)))
          fire(node, [[PointerEvent, "pointerleave"], [MouseEvent, "mouseleave"]], false, el);
      }
      fire(el, [[PointerEvent, "pointerover"], [MouseEvent, "mouseover"]], true, last);
      for (const node of now.filter(n => !old.includes(n)).reverse())
        fire(node, [[PointerEvent, "pointerenter"], [MouseEvent, "mouseenter"]], false, last);
      globalThis[KEY] = new WeakRef(el);
    }
    fire(el, [[PointerEvent, "pointermove"], [MouseEvent, "mousemove"]], true, null);
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
  // #11286: the keys page_press accepts; any other single printable character is also a key.
  const NAMED_KEYS = ["Enter","Tab","Escape","Backspace","Delete","ArrowUp","ArrowDown","ArrowLeft","ArrowRight",
    "Home","End","PageUp","PageDown","Space"];
  const MODIFIERS = ["Shift","Control","Alt","Meta"];
  function keyAllowed(key) {
    if (typeof key !== "string") return false;
    if (NAMED_KEYS.includes(key) || key === " ") return true;
    return [...key].length === 1 && !/[\p{C}\p{Z}]/u.test(key);
  }
  const modifiersAllowed = value => Array.isArray(value) && value.every(m => MODIFIERS.includes(m)) &&
    new Set(value).size === value.length;
  return {pageProbe, pageRead, pageClick, pageKeys, pageType, pagePress, pageSelect, pageScroll, pageSetFiles, pageHover,
    urlAllowed, keyAllowed, modifiersAllowed};
})();

// Why: the broker's page commands must be confined by the worker alone, whatever Chrome would allow.
// What: validates each command in the contract's check order (op, fields, email, grant, scope, tab),
//   then runs it against the explicit tab id. No path reads "the active tab".
// Test: tests/test_worker.mjs `a tab grant never reaches another tab`, `stale document refuses before the page runs`.
TrustyPages.commands = deps => {
  const {pageProbe, pageRead, pageClick, pageKeys, pageSelect, pageScroll, pageSetFiles, pageHover, urlAllowed,
    keyAllowed, modifiersAllowed} = TrustyPages;
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
    page_hover:{tab, document:doc, ref},
    page_type:{tab, document:doc, ref, text:v => typeof v === "string" && v.length <= 8192},
    // #11286: `ref` null presses the key on the focused element.
    page_press:{tab, document:doc, ref:v => v === null || ref(v), key:keyAllowed, modifiers:modifiersAllowed},
    page_select:{tab, document:doc, ref, value:v => typeof v === "string" && v.length <= 8192},
    // #11291: `ref` is a string exactly when `to` is "element"; `wait_ms` bounds the wait for new content.
    page_scroll:{tab, document:doc, to:v => SCROLL_TO.includes(v), ref:v => v === null || ref(v),
      wait_ms:v => Number.isInteger(v) && v >= 0 && v <= 5000},
    page_navigate:{tab, url:v => typeof v === "string" && v.length > 0 && v.length <= 8192},
    page_back:{tab}, page_forward:{tab}, page_reload:{tab},
    page_switch:{tab},
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

  async function landed(tabId, wait) {
    const settled = await wait.done;
    const {access, document, origin} = await probe(tabId);
    return {access, document, origin, settled};
  }

  // Why: history and reload act on the explicit tab through chrome.tabs, so they need no page access and
  //   no extension reload path (#11292). What: goBack/goForward reject when the tab has no such history
  //   entry, which answers NoHistory with nothing changed; reload keeps the cache.
  // Test: tests/test_worker.mjs `back, forward and reload report where the tab landed`.
  async function step(tabId, method) {
    const wait = settle(tabId);
    try {
      await (method === "reload" ? chrome.tabs.reload(tabId, {bypassCache:false}) : chrome.tabs[method](tabId));
    } catch {
      wait.cancel();
      try { await chrome.tabs.get(tabId); } catch { throw refuse("TabNotFound", {tab:tabId}); }
      throw refuse(method === "reload" ? "OutcomeUnknown" : "NoHistory", {tab:tabId});
    }
    return landed(tabId, wait);
  }

  // Runs the injected page commands only inside the lease's document; never reports work it did not do.
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
    const result = hit?.result, error = result?.error;
    if (["StaleElement","NotEditable","NotSelectable","NoSuchOption"].includes(error)) throw refuse(error, {tab:req.tab});
    if (result?.ok !== true || hit.documentId !== req.document) throw refuse("OutcomeUnknown");
    if (func !== pageSelect) return {document:req.document, ...(result.view || {})};
    // #11285: a select reply must say whether the value moved; anything else cannot show what ran.
    if (typeof result.changed !== "boolean" || typeof result.value !== "string") throw refuse("OutcomeUnknown");
    return {document:req.document, changed:result.changed, value:result.value.slice(0, 8192)};
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
    page_type:req => act(req, pageKeys, [req.ref, "type", req.text]),
    page_press:req => act(req, pageKeys, [req.ref, "press", {key:req.key, modifiers:req.modifiers}]),
    page_select:req => act(req, pageSelect, [req.ref, req.value]),
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
    page_hover:req => act(req, pageHover, [req.ref]),
    async page_navigate(req) {
      if (!urlAllowed(req.url)) throw refuse("InvalidUrl");
      const wait = settle(req.tab);
      try { await chrome.tabs.update(req.tab, {url:req.url}); } catch {
        wait.cancel();
        throw refuse("TabNotFound", {tab:req.tab});
      }
      await deps.remember(req.tab, new URL(req.url).origin);
      return landed(req.tab, wait);
    },
    page_back:req => step(req.tab, "goBack"),
    page_forward:req => step(req.tab, "goForward"),
    page_reload:req => step(req.tab, "reload"),
    // #11293: focus changes browser-wide state, so a tab grant may not switch even to its own tab.
    async page_switch(req, row) {
      if (row.scope === "tab") throw refuse("OutOfScope", {tab:req.tab});
      let shown;
      try { shown = await chrome.tabs.update(req.tab, {active:true}); } catch { throw refuse("TabNotFound", {tab:req.tab}); }
      let focused = true;
      try { await chrome.windows.update(shown.windowId, {focused:true}); } catch { focused = false; }
      return {window:shown.windowId, focused};
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
      const acting = ["page_click","page_type","page_press","page_select","page_scroll","page_navigate","page_back",
        "page_forward","page_reload","page_upload","page_hover"].includes(req.op);
      return {id:req.id, ok:false, error:acting ? "OutcomeUnknown" : "BrowserDisconnected"};
    }
    const fields = Object.fromEntries(Object.entries(error.fields || {}).filter(([k]) => RELAYED.includes(k)));
    return {id:req.id, ok:false, error:error.message, ...fields};
  }
  return {command, failure, probe, handles:op => Object.hasOwn(SPECS, op)};
};

