// Page operations for activation-code grants (#11239, docs/activation-codes.md).
// The worker loads this file with importScripts; tests load it into a page after pages_read.js, which holds
// the read (#11355). Top-level code touches no `chrome` API. The `page*` function declarations are injected by chrome.scripting.executeScript, so each one
// is self-contained: Chrome serializes the function source and runs it in the tab's isolated world.
if (!globalThis.TrustyPageRead && typeof importScripts === "function") importScripts("pages_read.js");
globalThis.TrustyPages = (() => {
  // Returns [origin, title, url] and changes nothing; the InjectionResult supplies the documentId.
  function pageProbe() { return [location.origin, document.title, location.href.slice(0, 2048)]; }

  // Why: a trusted click must land only on the element a read of this document reported (#11294); the
  //   worker then drives real input at the point this returns.
  // What: resolves the ref through the registry pageInspect installs (StaleElement when the element changed
  //   or left the top document through any shadow host or frame, #11355, #11290), scrolls it to the viewport
  //   centre and returns the centre in top-frame CSS pixels, the space CDP Input.dispatchMouseEvent takes:
  //   inside a same-origin frame each frame element's content-box offset is added up the chain, as
  //   pageRead's distance() does. The centre is hit-tested in each root up to the top document (shadow root,
  //   then host; frame document, then frame element); any other element on top answers Obscured. The
  //   scroll is instant: CSS `scroll-behavior:smooth` would otherwise leave the rect read mid-scroll.
  // Test: tests/test_pages_dom.py `test_target_refuses_stale_refs_and_covered_points`,
  //   `test_target_reads_the_rect_after_an_instant_scroll_under_smooth_css`,
  //   `test_trusted_click_lands_inside_iframes_and_shadow_roots`.
  function pageTarget(ref) {
    const el = globalThis.__trustyBrowserRefs?.resolve?.(ref);
    if (!el) return {error:"StaleElement"};
    el.scrollIntoView({block:"center", inline:"center", behavior:"instant"});
    const box = el.getBoundingClientRect();
    let x = box.left + box.width / 2, y = box.top + box.height / 2;
    for (let node = el; ;) {
      const root = node.getRootNode(), hit = root.elementFromPoint?.(x, y);
      if (!hit || !(hit === node || node.contains(hit))) return {error:"Obscured"};
      if (root.host) { node = root.host; continue; }
      const frame = root.defaultView?.frameElement;
      if (!frame || root === document) return {ok:true, x, y};
      const at = frame.getBoundingClientRect(), css = frame.ownerDocument.defaultView.getComputedStyle(frame);
      x += at.left + frame.clientLeft + parseFloat(css.paddingLeft);
      y += at.top + frame.clientTop + parseFloat(css.paddingTop);
      node = frame;
    }
  }

  // Why: a drag must start and end on elements a read of this document reported (#11288).
  // What: resolves both refs through the read registry (StaleElement when either does not resolve, including
  //   one that left the top document through a shadow host or frame), then dispatches dragstart, drag and
  //   dragend on the source and dragenter, dragover and drop on the target, sharing one DataTransfer. The drop
  //   fires only when the target cancels dragover (the HTML drop-target rule); otherwise dragleave fires. A
  //   cancelled dragstart ends the drag at once. The events are synthetic (isTrusted false); see #11312.
  // Test: tests/test_pages_dom.py `test_drag_fires_the_drag_sequence_and_drops`,
  //   `test_drag_reaches_a_draggable_inside_an_open_shadow_root`.
  function pageDrag(ref, target) {
    const resolve = globalThis.__trustyBrowserRefs?.resolve;
    const source = resolve?.(ref), sink = resolve?.(target);
    if (!source || !sink) return {error:"StaleElement"};
    if (source === sink || source.contains(sink)) return {error:"NotDraggable"};
    const point = el => {
      el.scrollIntoView({block:"center", inline:"center"});
      const box = el.getBoundingClientRect();
      return {bubbles:true, cancelable:true, composed:true, view:el.ownerDocument.defaultView,
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

  // Why: page_type and page_press must reach the page as keystrokes: handlers listen for keydown, and a
  //   form submits on Enter (#11286). Synthetic key events carry no default action, so this emulates it.
  // What: action "type" checks that `ref` is a supported editable target (else NotEditable), focuses it,
  //   selects its content and presses each character of `input` ("\n" or "\r" is Enter, "\t" is Tab).
  //   Action "press" focuses `ref` when given (null keeps the focused element) and presses
  //   `input.key` with `input.modifiers`. Each press fires modifier keydowns, keydown, keypress (printable
  //   keys and Enter without Control or Meta), the default action unless a handler cancelled, keyup, then
  //   modifier keyups. Self-contained: Chrome serializes this one function into the page.
  //   Text a maxlength cut answers ok with truncated, typed and accepted; text a cancelled beforeinput
  //   refused answers TextDropped with typed, accepted and entered (#11352).
  // Test: tests/test_pages_dom.py `test_type_emits_key_events_and_enter_submits_the_form`,
  //   `test_type_reports_text_dropped_by_maxlength_or_a_cancelled_beforeinput`,
  //   `test_press_tab_and_shift_tab_follow_the_focus_order`,
  //   `test_press_edits_fields_and_honours_chords_and_prevent_default`,
  //   `test_type_sets_values_and_refuses_non_editable`.
  function pageKeys(ref, action, input) {
    let el = null;
    if (ref !== null) {
      el = globalThis.__trustyBrowserRefs?.resolve?.(ref);
      if (!el) return {error:"StaleElement"};
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
    // #11352: text a maxlength cut or a cancelled beforeinput refused, so the reply never claims it landed.
    const drops = {cut:false, cancelled:false, typed:0, entered:""};
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
      bubbles:true, cancelable:true, composed:true, view:node.ownerDocument.defaultView}));
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
      if (data) drops.typed += [...data].length;
      if (data && node.maxLength >= 0 && next.length > node.maxLength) { drops.cut = true; return; }
      if (!node.dispatchEvent(new InputEvent("beforeinput", {bubbles:true, cancelable:true, composed:true,
        inputType, data:data || null}))) { if (data) drops.cancelled = true; return; }
      drops.entered += data;
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
      if (node.isContentEditable) { node.ownerDocument.execCommand("insertText", false, data); return; }
      const [from, to] = span(node);
      edit(node, from, to, data, "insertText");
    }
    function remove(node, backward) {
      if (!writable(node)) return;
      if (node.isContentEditable) { node.ownerDocument.execCommand(backward ? "delete" : "forwardDelete"); return; }
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
    // The reply: ok (with the counts when maxlength cut the text), or TextDropped when a page handler refused text.
    const outcome = () => {
      const accepted = [...drops.entered].length;
      if (drops.cancelled) return {error:"TextDropped", typed:drops.typed, accepted, entered:drops.entered.slice(0, 8192)};
      return drops.cut ? {ok:true, truncated:true, typed:drops.typed, accepted} : {ok:true};
    };
    if (action === "press") {
      pressOne(input.key, input.modifiers, node);
      return outcome();
    }
    // "type": the text replaces the element's content, as one keystroke per character.
    if (el.isContentEditable) {
      // #11290: an element inside an iframe is selected and edited through its own document.
      const doc = el.ownerDocument, range = doc.createRange();
      range.selectNodeContents(el);
      doc.getSelection().removeAllRanges();
      doc.getSelection().addRange(range);
      if (!input) doc.execCommand("delete");
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
    return outcome();
  }

  // Why: offscreen content and infinite-scroll feeds load only when the page scrolls, and an agent needs
  //   to know when a feed has stopped growing (#11291).
  // What: `to` "down"/"up" scrolls the document by 90% of the viewport, "top"/"bottom" to an end, and
  //   "element" brings `ref` (StaleElement when its element changed) to the centre. It then waits up to
  //   `waitMs` for added nodes, returning 150 ms after they stop. The view reports the scroll position,
  //   whether the document grew and whether it now sits at the bottom. Refs stay valid: no read runs.
  // Test: tests/test_pages_dom.py `test_scroll_drives_an_infinite_feed_until_it_ends`,
  //   `test_scroll_to_element_and_stale_refs_after_dom_change`.
  async function pageScroll(to, ref, waitMs) {
    let el = null;
    if (to === "element") {
      el = globalThis.__trustyBrowserRefs?.resolve?.(ref);
      if (!el) return {error:"StaleElement"};
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

  // Why: sites that need native input or user activation reject synthetic clicks (#11294).
  // What: drives Chrome DevTools Protocol input through `send(method, params)`, so every event the page
  //   sees is trusted. A click is move, press, release at the point. Typing stays on pageKeys.
  // Test: tests/test_pages_dom.py `test_trusted_click_arrives_trusted`.
  const trusted = {
    async click(send, {x, y}) {
      const at = {x, y, button:"left", clickCount:1};
      await send("Input.dispatchMouseEvent", {type:"mouseMoved", x, y});
      await send("Input.dispatchMouseEvent", {type:"mousePressed", ...at, buttons:1});
      await send("Input.dispatchMouseEvent", {type:"mouseReleased", ...at, buttons:0});
    },
    // A native dialog blocks the page until answered: an alert is acknowledged, any other follows `policy`.
    accepts:(type, policy) => type === "alert" || policy === "accept",
  };

  // Why: a file input cannot be typed into; the agent must hand the page real file bytes (#11289).
  // What: builds File objects from base64 data, sets them through a DataTransfer, then fires bubbling
  //   input and change as a user's pick would. A target that is not an enabled <input type=file>, or more
  //   than one file for a single-file input, answers NotEditable and changes nothing.
  // Test: tests/test_pages_dom.py `test_set_files_reaches_the_page_and_the_server`,
  //   `test_set_files_refuses_other_targets`.
  function pageSetFiles(ref, files) {
    const el = globalThis.__trustyBrowserRefs?.resolve?.(ref);
    if (!el) return {error:"StaleElement"};
    const isFile = el.localName === "input" && (el.getAttribute("type") || "").toLowerCase() === "file";
    if (!isFile || el.disabled || (files.length > 1 && !el.multiple)) return {error:"NotEditable"};
    const transfer = new DataTransfer();
    // PR #11305 delta review: bad base64 answers NotEditable, never a thrown error.
    try {
      for (const {name, type, data} of files) {
        const raw = atob(data), bytes = new Uint8Array(raw.length);
        for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
        transfer.items.add(new File([bytes], name, {type}));
      }
    } catch {
      return {error:"NotEditable"};
    }
    el.files = transfer.files;
    el.dispatchEvent(new Event("input", {bubbles:true, composed:true}));
    el.dispatchEvent(new Event("change", {bubbles:true}));
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
    const el = globalThis.__trustyBrowserRefs?.resolve?.(ref);
    if (!el) return {error:"StaleElement"};
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

  // Why: menus and tooltips open on pointer entry, and an agent had no way to move the pointer (#11287).
  // What: resolves the ref, scrolls it into view and moves the synthetic pointer from the element this
  //   page last hovered to it, across shadow roots and same-origin frames: out/leave on the old path, over/enter
  //   on the new one (enter from the outermost ancestor down, leave from the innermost up), then
  //   pointermove/mousemove at the centre. Each event's `view` is the window of its own target's document.
  //   Hovering the same element again sends only the moves. The events are untrusted, so CSS `:hover` does
  //   not match; that needs trusted input (#11294).
  // Test: tests/test_pages_dom.py `test_hover_fires_enter_over_and_move_then_leaves_the_old_target`,
  //   `test_hover_fixture_reveals_the_hover_action`,
  //   `test_hover_reaches_an_element_inside_a_same_origin_iframe`.
  function pageHover(ref) {
    const el = globalThis.__trustyBrowserRefs?.resolve?.(ref);
    if (!el) return {error:"StaleElement"};
    el.scrollIntoView({block:"center", inline:"center"});
    const box = el.getBoundingClientRect();
    const at = {cancelable:true, composed:true, pointerType:"mouse", isPrimary:true,
      clientX:box.left + box.width / 2, clientY:box.top + box.height / 2};
    const KEY = "__trustyBrowserHover";
    let last = globalThis[KEY]?.deref();
    if (last && !last.isConnected) last = null;
    // #11300: the chain crosses shadow hosts and same-origin frame elements, so a move from the top page
    // into a frame leaves only what the pointer left and enters the frame element and the target.
    const parent = node => node.parentElement || node.parentNode?.host ||
      (node.parentNode === node.ownerDocument ? node.ownerDocument.defaultView?.frameElement : null);
    const path = node => { const out = []; for (; node; node = parent(node)) out.push(node); return out; };
    const fire = (target, kinds, bubbles, relatedTarget) => {
      const view = target.ownerDocument.defaultView;
      for (const [Kind, type] of kinds)
        target.dispatchEvent(new Kind(type, {...at, view, bubbles, relatedTarget}));
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
  const {pageInspect, pageRead, pageFind, pageFrames} = globalThis.TrustyPageRead;
  return {pageProbe, pageInspect, pageRead, pageFind, pageFrames, pageTarget, pageHover, pageKeys, pageType, pagePress,
    pageScroll, pageSelect, pageSetFiles, pageDrag, trusted, urlAllowed, keyAllowed, modifiersAllowed};
})();

// Why: the broker's page commands must be confined by the worker alone, whatever Chrome would allow.
// What: validates each command in the contract's check order (op, fields, email, grant, scope, tab),
//   then runs it against the explicit tab id. No path reads "the active tab".
// Test: tests/test_worker.mjs `a tab grant never reaches another tab`, `stale document refuses before the page runs`.
TrustyPages.commands = deps => {
  const {pageProbe, pageInspect, pageFrames, pageTarget, pageHover, pageKeys, pageScroll, pageSelect, pageSetFiles, pageDrag, trusted, urlAllowed,
    keyAllowed, modifiersAllowed} = TrustyPages;
  // #11352: a TextDropped refusal also says what was typed and what landed.
  const RELAYED = ["tab","origin","access","document","typed","accepted","entered"];
  const REASONS = ["revoked","tab_closed","browser_gone","unenrolled","audit","closed","expired"];
  const REF = /^[0-9]{1,9}\.[0-9]{1,6}$/;
  const tab = v => Number.isInteger(v) && v >= 0;
  const doc = v => v === null || (typeof v === "string" && v.length > 0 && v.length <= 8192);
  const ref = v => typeof v === "string" && REF.test(v);
  // #11301: a ref from a cross-origin child frame is "<frameKey>:<epoch>.<n>"; only scope_ref and within_ref take it.
  const FRAME_REF = /^([0-9]{1,9}):([0-9]{1,9}\.[0-9]{1,6})$/;
  const scopeRef = v => v === null || ref(v) || (typeof v === "string" && FRAME_REF.test(v));
  const bool = v => typeof v === "boolean";
  const term = v => v === null || (typeof v === "string" && v.length > 0 && v.length <= 200);
  const SCROLL_TO = ["down","up","top","bottom","element"];
  // #11289: at most 10 files and 700000 base64 characters, so a frame stays under the 1 MiB native limit.
  const B64 = /^[A-Za-z0-9+/]*={0,2}$/;
  const files = v => Array.isArray(v) && v.length >= 1 && v.length <= 10 &&
    v.reduce((sum, f) => sum + (typeof f?.data === "string" ? f.data.length : Infinity), 0) <= 700000 &&
    v.every(f => f && typeof f === "object" && Object.keys(f).length === 3 &&
      typeof f.name === "string" && f.name.length > 0 && f.name.length <= 255 && !/[\\/]/.test(f.name) &&
      typeof f.type === "string" && f.type.length <= 255 &&
      typeof f.data === "string" && f.data.length % 4 === 0 && B64.test(f.data));
  const dialog = v => v === "accept" || v === "dismiss";
  const SPECS = {
    page_tabs:{}, page_tab:{tab}, page_click:{tab, document:doc, ref, dialog},
    page_drag:{tab, document:doc, ref, target:ref},
    // #11356: the broker always sends every read control.
    page_read:{tab, document:doc, scope_ref:scopeRef, viewport_only:bool, include_text:bool,
      max_chars:v => Number.isInteger(v) && v >= 1000 && v <= 250000},
    // #11357: at least one of role, name and text; within_ref limits the search to a subtree.
    page_find:{tab, document:doc, role:term, name:term, text:term, within_ref:scopeRef,
      limit:v => Number.isInteger(v) && v >= 1 && v <= 50},
    page_hover:{tab, document:doc, ref},
    page_type:{tab, document:doc, ref, text:v => typeof v === "string" && v.length <= 8192},
    // #11291: `ref` is a string exactly when `to` is "element"; `wait_ms` bounds the wait for new content.
    page_scroll:{tab, document:doc, to:v => SCROLL_TO.includes(v), ref:v => v === null || ref(v),
      wait_ms:v => Number.isInteger(v) && v >= 0 && v <= 5000},
    // #11286: `ref` null presses the key on the focused element.
    page_press:{tab, document:doc, ref:v => v === null || ref(v), key:keyAllowed, modifiers:modifiersAllowed},
    page_select:{tab, document:doc, ref, value:v => typeof v === "string" && v.length <= 8192},
    page_navigate:{tab, url:v => typeof v === "string" && v.length > 0 && v.length <= 8192},
    page_upload:{tab, document:doc, ref, files}, page_downloads:{tab},
    page_back:{tab}, page_forward:{tab}, page_reload:{tab},
    page_switch:{tab},
    grant_end:{reason:v => REASONS.includes(v), last:v => typeof v === "boolean"},
  };
  const refuse = (error, fields = {}) => Object.assign(new Error(error), {fields});
  const none = {access:false, document:null, origin:null, title:null};

  // `withUrl` adds the top document's url (#11301); page_tab's PROBE result carries none.
  async function probe(tabId, withUrl = false) {
    let hit;
    try {
      [hit] = await chrome.scripting.executeScript({target:{tabId, frameIds:[0]}, func:pageProbe});
    } catch { return none; }
    if (!Array.isArray(hit?.result) || typeof hit.documentId !== "string" || typeof hit.result[0] !== "string") return none;
    await deps.remember(tabId, hit.result[0]);
    const seen = {access:true, document:hit.documentId, origin:hit.result[0], title:String(hit.result[1]).slice(0, 200)};
    return withUrl ? {...seen, url:String(hit.result[2] ?? "").slice(0, 2048)} : seen;
  }
  async function needed(tabId) { return refuse("PermissionNeeded", {tab:tabId, origin:await deps.originOf(tabId)}); }

  // #11301: cross-origin child frames for page_read and page_find (docs/activation-codes.md, "Element addressing").
  const CHILD_FRAMES = 8, DENIED_FRAMES = 20, READ_LIMIT = 262144 - 512;
  // "<tab>:<frameKey>" -> {document, origin, url} of the child document a read reported, so a frame-scoped ref
  // names that document only.
  const frameDocs = new Map();
  // True only when Chrome holds host permission for `origin` itself; activeTab and a parent's grant do not count.
  async function granted(origin) {
    let url;
    try { url = new URL(origin); } catch { return false; }
    if (url.protocol !== "https:" && url.protocol !== "http:") return false;
    try {
      return await chrome.permissions.contains({origins:[url.protocol + "//" + url.hostname + "/*"]}) === true;
    } catch { return false; }
  }
  // Why: a child frame's content may reach the agent only under host permission for that child's own origin
  //   (#11301); the worker holds no webNavigation permission, so it finds frames by injecting.
  // What: injects pageFrames into every frame. Each root child frame (its parent is cross-origin) is checked on
  //   its own origin: an opaque origin, a missing permission or more than CHILD_FRAMES granted frames deny it.
  //   Declared cross-origin children Chrome did not inject into are denied when their origin is not granted.
  //   Answers null when the top frame did not answer, else {top, frames, denied}; it reads no child content.
  // Test: tests/extension/test_frames.mjs `a granted child frame is read with attribution`,
  //   `a child frame without host permission is denied with no content`, `grants are not inherited by grandchildren`.
  async function framesOf(tabId) {
    let hits;
    try { hits = await chrome.scripting.executeScript({target:{tabId, allFrames:true}, func:pageFrames}); } catch { return null; }
    const top = Array.isArray(hits) ? hits.find(h => h?.frameId === 0) : null;
    if (typeof top?.documentId !== "string" || typeof top.result?.origin !== "string") return null;
    const frames = [], denied = new Map(), seen = new Set();
    const deny = (origin, reason) => { if (denied.size < DENIED_FRAMES && !denied.has(origin)) denied.set(origin, reason); };
    const listed = h => Array.isArray(h.result.children) ? h.result.children.filter(o => typeof o === "string") : [];
    const declared = listed(top);
    const roots = hits.filter(h => Number.isInteger(h?.frameId) && h.frameId !== 0 && h.result?.root === true &&
      typeof h.documentId === "string" && typeof h.result.origin === "string").sort((a, b) => a.frameId - b.frameId);
    for (const h of roots) {
      const origin = h.result.origin;
      seen.add(origin);
      if (!/^https?:\/\//.test(origin)) deny(origin, "opaque");
      else if (!await granted(origin)) deny(origin, "no_permission");
      else if (frames.length === CHILD_FRAMES) deny(origin, "frame_limit");
      else {
        frames.push({id:h.frameId, origin, url:String(h.result.url ?? "").slice(0, 2048), document:h.documentId});
        declared.push(...listed(h));
      }
    }
    for (const origin of declared) {
      if (seen.has(origin)) continue;
      seen.add(origin);
      if (!await granted(origin)) deny(origin, "no_permission");
    }
    return {top, frames, denied:[...denied].map(([origin, reason]) => ({origin, reason}))};
  }
  const frameKey = (tab, id) => tab + ":" + id;
  // A child entry's ref (and its container's) gain the frame key; every entry names its frame.
  const inFrame = (id, entry) => id === 0 ? {...entry, frame:0} : {...entry, ref:id + ":" + entry.ref, frame:id,
    ...(entry.in ? {in:{...entry.in, ref:id + ":" + entry.in.ref}} : {})};
  const size = value => new TextEncoder().encode(JSON.stringify(value)).length;
  // Cuts text first, then entries from the end (child frames come last), until `out` fits `budget`.
  function fit(out, list, budget) {
    for (let over = size(out) - budget; over > 0; over = size(out) - budget) {
      out.truncated = true;
      if (out.text) out.text = out.text.slice(0, Math.max(0, out.text.length - over));
      else if (out[list].length) out[list].pop();
      else break;
    }
    return out;
  }
  // Runs pageInspect in one child document only; null when it navigated or answered another origin.
  async function childInspect(tabId, frame, mode, options) {
    let hit;
    try {
      [hit] = await chrome.scripting.executeScript({target:{tabId, documentIds:[frame.document]}, func:pageInspect,
        args:[mode, 512, {...options, frame:true}]});
    } catch { return null; }
    const result = hit?.result;
    if (!result || result.error || hit.documentId !== frame.document || result.origin !== frame.origin) return null;
    return result;
  }
  // Why: one result must say which frame each entry came from and which frames were withheld (#11301).
  // What: adds `frames` ({id, origin, url, document}, the top first as id 0) and `denied_frames`, reads each
  //   granted child with its share of the budget, appends its entries with frame-scoped refs, records
  //   frameKey -> documentId for later scoped reads, then bounds the list to `limit` and the bytes to `budget`.
  // Test: tests/extension/test_frames.mjs `a granted child frame is read with attribution`,
  //   `the frame count and byte share stay inside the result budget`,
  //   `include_text reads each granted child frame's text within its share`.
  async function withFrames(req, base, plan, mode, options, limit, budget) {
    const list = mode === "read" ? "elements" : "matches";
    for (const key of [...frameDocs.keys()]) if (key.startsWith(req.tab + ":")) frameDocs.delete(key);
    const url = base.url ?? String(plan.top.result.url ?? "").slice(0, 2048);
    const out = {...base, [list]:base[list].map(e => inFrame(0, e))};
    const frames = [{id:0, origin:base.origin, url, document:base.document}], denied = [...plan.denied];
    for (const frame of plan.frames) {
      const child = await childInspect(req.tab, frame, mode, options);
      if (!Array.isArray(child?.[list])) {
        if (!denied.some(d => d.origin === frame.origin)) denied.push({origin:frame.origin, reason:"navigated"});
        continue;
      }
      frames.push(frame);
      frameDocs.set(frameKey(req.tab, frame.id), {document:frame.document, origin:frame.origin, url:frame.url});
      out[list].push(...child[list].map(e => inFrame(frame.id, e)));
      // #11284: a child's text joins the result's text under a frame marker; it was read within its share.
      if (mode === "read" && child.text) out.text += "\n\n[frame " + frame.id + "]\n" + child.text;
      if (child.truncated) out.truncated = true;
    }
    if (out[list].length > limit) { out[list].length = limit; out.truncated = true; }
    return fit({...out, frames, denied_frames:denied.slice(0, DENIED_FRAMES)}, list, budget);
  }
  // Why: a frame-scoped ref must name the child document it was read from, never one the frame navigated to.
  // What: looks up the frame key a read recorded (StaleElement when unknown), re-checks the child origin's
  //   permission, and injects only into that documentId: a navigated frame's document is gone, so Chrome
  //   refuses and the ref answers StaleElement.
  // Test: tests/extension/test_frames.mjs `a child ref is refused once its frame navigated`.
  async function frameScoped(req, mode, key, options) {
    const [, id, local] = FRAME_REF.exec(req[key]);
    const known = frameDocs.get(frameKey(req.tab, id));
    if (!known) throw refuse("StaleElement", {tab:req.tab});
    const top = await probe(req.tab, true);
    if (!top.access || !await granted(known.origin)) throw await needed(req.tab);
    const frame = {id:Number(id), ...known};
    const result = await childInspect(req.tab, frame, mode, {...options, [key]:local});
    const list = mode === "read" ? "elements" : "matches";
    if (!Array.isArray(result?.[list])) {
      frameDocs.delete(frameKey(req.tab, id));
      throw refuse("StaleElement", {tab:req.tab});
    }
    const head = {document:top.document, origin:top.origin};
    const out = mode === "read" ? {...head, url:top.url, title:top.title, text:result.text, elements:[],
      truncated:result.truncated} : {...head, matches:[], truncated:result.truncated === true};
    out[list] = result[list].map(e => inFrame(frame.id, e));
    const frames = [{id:0, origin:top.origin, url:top.url, document:top.document},
      {id:frame.id, origin:known.origin, url:known.url, document:known.document}];
    return fit({...out, frames, denied_frames:[]}, list, mode === "read" ? Math.min(READ_LIMIT, req.max_chars) : READ_LIMIT);
  }

  // Resolves true at the tab's next "complete" (after a "loading" unless `loading` is already true),
  // false after `ms` or on cancel.
  function settle(tabId, ms = 5000, loading = false) {
    let finish;
    const done = new Promise(resolve => {
      const listener = (id, info) => {
        if (id !== tabId) return;
        if (info.status === "loading") loading = true;
        if (loading && info.status === "complete") finish(true);
      };
      const timer = setTimeout(() => finish(false), ms);
      finish = value => { clearTimeout(timer); chrome.tabs.onUpdated.removeListener(listener); resolve(value); };
      chrome.tabs.onUpdated.addListener(listener);
    });
    return {done, cancel:() => finish(false)};
  }

  // Why: before a navigation commits, executeScript still runs in the old document, so a read would
  //   return it as current (#11354).
  // What: while the tab's status is "loading", waits up to 3 s for "complete"; still loading answers
  //   NavigationPending. Listens before re-checking the status, so a completion between the two is not lost.
  // Test: tests/test_worker.mjs `read during an unsettled navigation never returns the old document`.
  async function landed(tabId) {
    const loading = async () => {
      try { return (await chrome.tabs.get(tabId)).status === "loading"; } catch { throw refuse("TabNotFound", {tab:tabId}); }
    };
    if (!await loading()) return;
    const wait = settle(tabId, 3000, true);
    if (!await loading()) { wait.cancel(); return; }
    if (!await wait.done) throw refuse("NavigationPending", {tab:tabId});
  }

  async function arrived(tabId, wait) {
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
    return arrived(tabId, wait);
  }

  // Runs the injected hover/type/press/scroll/select/upload only inside the lease's document; never reports work it
  //   did not do.
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
    const result = hit?.result, error = result?.error;
    if (["StaleElement","NotEditable","NotSelectable","NoSuchOption","NotDraggable"].includes(error)) {
      throw refuse(error, {tab:req.tab});
    }
    const counts = Number.isInteger(result?.typed) && Number.isInteger(result?.accepted);
    if (error === "TextDropped" && counts && typeof result.entered === "string") {
      throw refuse(error, {tab:req.tab, typed:result.typed, accepted:result.accepted,
        entered:result.entered.slice(0, 8192)});
    }
    if (result?.ok !== true || hit.documentId !== req.document) throw refuse("OutcomeUnknown");
    // #11352: maxlength cut the typed text; the typing still succeeded.
    if (func === pageKeys && result.truncated === true) {
      if (!counts) throw refuse("OutcomeUnknown");
      return {document:req.document, truncated:true, typed:result.typed, accepted:result.accepted};
    }
    if (func === pageDrag) {
      if (typeof result.dropped !== "boolean") throw refuse("OutcomeUnknown");
      return {document:req.document, dropped:result.dropped};
    }
    if (func !== pageSelect) return {document:req.document, ...(result.view || {})};
    // #11285: a select reply must say whether the value moved; anything else cannot show what ran.
    if (typeof result.changed !== "boolean" || typeof result.value !== "string") throw refuse("OutcomeUnknown");
    return {document:req.document, changed:result.changed, value:result.value.slice(0, 8192)};
  }

  // Why: a click must arrive trusted, yet run only inside the lease's document and never report work it
  //   did not do (#11294).
  // What: probes first and attaches chrome.debugger only to an accessible tab still on the lease's document
  //   (before targeting, since Chrome's debugging bar resizes the viewport), injects pageTarget into that
  //   document only, then drives CDP input at the point it returns. Before every input event it re-probes
  //   and refuses DocumentChanged if the document or origin moved. Any
  //   native dialog is answered by `req.dialog` (an alert is always acknowledged); an answer Chrome
  //   rejects, or a command outlasting its time bound, answers OutcomeUnknown. It waits 100 ms in the page
  //   for a late dialog, then always detaches; after that its leftover work sends no CDP command. The
  //   reply names the last dialog seen.
  // Test: tests/test_worker.mjs `click drives trusted input and answers dialogs`,
  //   `stale document refuses before the page runs`, `trusted input rechecks the document before every event`,
  //   `a no-access tab is never debugger-attached`, `a failed dialog answer or a hung command answers OutcomeUnknown`,
  //   `a timed-out click sends no input after a later attach`.
  async function act(req, drive) {
    const changed = now => refuse("DocumentChanged", {tab:req.tab, access:now.access, document:now.document});
    if (req.document === null) throw changed(await probe(req.tab));
    const first = await probe(req.tab);
    if (!first.access || first.document !== req.document) throw changed(first);
    // #11294: once act returns, its leftover work must not reach a later attachment to this tab.
    let aborted = false;
    const debuggee = {tabId:req.tab}, raw = async (method, params = {}) => {
      if (aborted) throw refuse("OutcomeUnknown");
      return chrome.debugger.sendCommand(debuggee, method, params);
    };
    // Every input event runs only while the lease's document and origin are still the tab's top document.
    const send = async (method, params = {}) => {
      if (method.startsWith("Input.")) {
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
      timer = setTimeout(() => reject(refuse("OutcomeUnknown")), 5000);
    });
    try {
      await Promise.race([bound, steps()]);
    } finally {
      aborted = true;
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
          func:pageTarget, args:[req.ref]});
      } catch {
        const now = await probe(req.tab);
        // The lease's document is still current, so the failure came from the page: the outcome is unknown.
        if (now.access && now.document === req.document) throw refuse("OutcomeUnknown");
        throw changed(now);
      }
      const point = hit?.result, error = point?.error;
      if (["StaleElement","Obscured"].includes(error)) throw refuse(error, {tab:req.tab});
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
      await landed(req.tab);
      const options = {scope_ref:req.scope_ref, max_chars:req.max_chars, viewport_only:req.viewport_only,
        include_text:req.include_text};
      if (FRAME_REF.test(req.scope_ref ?? "")) return frameScoped(req, "read", "scope_ref", options);
      // #11301: an unscoped read also reads the granted cross-origin child frames, which share half the budget.
      const plan = req.scope_ref === null ? await framesOf(req.tab) : null;
      const many = plan !== null && (plan.frames.length > 0 || plan.denied.length > 0);
      const budget = Math.min(READ_LIMIT, req.max_chars);
      const share = many ? Math.floor(budget / 2 / Math.max(1, plan.frames.length)) : 0;
      let hit;
      try {
        [hit] = await chrome.scripting.executeScript({target:{tabId:req.tab, frameIds:[0]}, func:pageInspect,
          args:["read", 512, many ? {...options, max_chars:budget - share * plan.frames.length} : options]});
      } catch { throw await needed(req.tab); }
      const result = hit?.result;
      if (result?.error === "StaleElement") throw refuse("StaleElement", {tab:req.tab});
      if (!result || typeof hit.documentId !== "string" || typeof result.origin !== "string") throw await needed(req.tab);
      await deps.remember(req.tab, result.origin);
      const base = {document:hit.documentId, origin:result.origin, url:result.url, title:result.title,
        text:result.text, elements:result.elements, truncated:result.truncated};
      if (!many || hit.documentId !== plan.top.documentId) return base;
      return withFrames(req, base, plan, "read", {scope_ref:null, max_chars:Math.max(1, share),
        viewport_only:req.viewport_only, include_text:req.include_text}, 500, budget);
    },
    // #11357: a read like page_read, of matches only.
    async page_find(req) {
      if (req.role === null && req.name === null && req.text === null) throw refuse("InvalidRequest");
      await landed(req.tab);
      const terms = {role:req.role, name:req.name, text:req.text, within_ref:req.within_ref, limit:req.limit};
      if (FRAME_REF.test(req.within_ref ?? "")) return frameScoped(req, "find", "within_ref", terms);
      const plan = req.within_ref === null ? await framesOf(req.tab) : null;
      let hit;
      try {
        [hit] = await chrome.scripting.executeScript({target:{tabId:req.tab, frameIds:[0]}, func:pageInspect,
          args:["find", 512, terms]});
      } catch { throw await needed(req.tab); }
      const result = hit?.result;
      if (result?.error === "StaleElement") throw refuse("StaleElement", {tab:req.tab});
      if (!result || typeof hit.documentId !== "string" || typeof result.origin !== "string" ||
          !Array.isArray(result.matches)) throw await needed(req.tab);
      await deps.remember(req.tab, result.origin);
      const base = {document:hit.documentId, origin:result.origin, matches:result.matches, truncated:result.truncated === true};
      if (!plan || !(plan.frames.length || plan.denied.length) || hit.documentId !== plan.top.documentId) return base;
      return withFrames(req, base, plan, "find", {...terms, within_ref:null}, req.limit, READ_LIMIT);
    },
    page_click:req => act(req, trusted.click),
    page_type:req => inject(req, pageKeys, [req.ref, "type", req.text]),
    page_press:req => inject(req, pageKeys, [req.ref, "press", {key:req.key, modifiers:req.modifiers}]),
    page_scroll(req) {
      if ((req.to === "element") !== (req.ref !== null)) throw refuse("InvalidRequest");
      return inject(req, pageScroll, [req.to, req.ref, req.wait_ms]);
    },
    page_upload:req => inject(req, pageSetFiles, [req.ref, req.files]),
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
    page_select:req => inject(req, pageSelect, [req.ref, req.value]),
    page_hover:req => inject(req, pageHover, [req.ref]),
    page_drag:req => inject(req, pageDrag, [req.ref, req.target]),
    async page_navigate(req) {
      if (!urlAllowed(req.url)) throw refuse("InvalidUrl");
      const wait = settle(req.tab);
      try { await chrome.tabs.update(req.tab, {url:req.url}); } catch {
        wait.cancel();
        throw refuse("TabNotFound", {tab:req.tab});
      }
      await deps.remember(req.tab, new URL(req.url).origin);
      return arrived(req.tab, wait);
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
      const acting = ["page_click","page_hover","page_type","page_press","page_select","page_scroll","page_navigate",
        "page_upload","page_back","page_forward","page_reload","page_drag"].includes(req.op);
      return {id:req.id, ok:false, error:acting ? "OutcomeUnknown" : "BrowserDisconnected"};
    }
    const fields = Object.fromEntries(Object.entries(error.fields || {}).filter(([k]) => RELAYED.includes(k)));
    return {id:req.id, ok:false, error:error.message, ...fields};
  }
  return {command, failure, probe, handles:op => Object.hasOwn(SPECS, op)};
};

