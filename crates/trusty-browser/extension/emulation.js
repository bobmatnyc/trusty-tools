// Leased-tab emulation over a held chrome.debugger attachment (#11371).
// Why: Chrome drops CDP emulation overrides when the debugger session detaches, so an emulated tab keeps
//   one attachment until its state returns to the defaults; and no emulation may outlive the lease or the
//   grant that set it.
// What: `apply` sets the whole state of a tab (JavaScript, prefers-color-scheme, viewport) and holds the
//   attachment; the defaults reset the tab and detach. `attach`/`detach` let pages.js act() share a held
//   attachment instead of failing with DebuggerBusy. `reset`, `resetGrant` and `resetAll` restore the
//   defaults and detach: on the broker's reset, `grant_end`, a command for the tab under another grant,
//   a dropped native port, and (`sweep`) a worker start. Top-level code touches no `chrome` API.
// Test: tests/extension/test_emulation.mjs.
globalThis.TrustyEmulation = (() => {
  const held = new Map();
  const DEFAULTS = {javascript:true, color_scheme:null, width:0, height:0};
  const side = v => Number.isInteger(v) && v >= 0 && v <= 10000;
  const spec = {javascript:v => typeof v === "boolean", color_scheme:v => v === null || v === "light" || v === "dark",
    width:side, height:side};
  const refuse = (error, fields = {}) => Object.assign(new Error(error), {fields});
  const plain = s => s.javascript && s.color_scheme === null && s.width === 0 && s.height === 0;
  let listening = false;
  function listen() {
    if (listening || !chrome.debugger?.onDetach) return;
    listening = true;
    // Chrome detached (tab closed, or the user cancelled the debugging bar): the overrides are gone with it.
    chrome.debugger.onDetach.addListener(source => held.delete(source.tabId));
  }
  async function send(tabId, s) {
    const cdp = (method, params = {}) => chrome.debugger.sendCommand({tabId}, method, params);
    await cdp("Emulation.setScriptExecutionDisabled", {value:!s.javascript});
    await cdp("Emulation.setEmulatedMedia", {features:[{name:"prefers-color-scheme", value:s.color_scheme ?? ""}]});
    if (s.width > 0) {
      await cdp("Emulation.setDeviceMetricsOverride", {width:s.width, height:s.height, deviceScaleFactor:0, mobile:false});
    } else await cdp("Emulation.clearDeviceMetricsOverride");
  }
  async function reset(tabId) {
    if (!held.has(tabId)) return;
    held.delete(tabId);
    // Detaching alone drops the overrides; the explicit defaults cover a Chrome that keeps them.
    await send(tabId, DEFAULTS).catch(() => {});
    await chrome.debugger.detach({tabId}).catch(() => {});
  }
  async function apply(req) {
    const state = {javascript:req.javascript, color_scheme:req.color_scheme, width:req.width, height:req.height};
    if ((state.width === 0) !== (state.height === 0)) throw refuse("InvalidRequest");
    if (plain(state)) {
      await reset(req.tab);
      return state;
    }
    listen();
    if (!held.has(req.tab)) {
      try { await chrome.debugger.attach({tabId:req.tab}, "1.3"); } catch { throw refuse("DebuggerBusy", {tab:req.tab}); }
      held.set(req.tab, {grant:req.grant});
    }
    held.get(req.tab).grant = req.grant;
    try { await send(req.tab, state); } catch {
      await reset(req.tab);
      throw refuse("OutcomeUnknown");
    }
    return state;
  }
  return {
    spec, apply, reset,
    held:tabId => held.has(tabId),
    // A command for an emulated tab under another grant resets it first: no state reaches the next holder.
    async guard(req) {
      if (Number.isInteger(req.tab) && held.has(req.tab) && held.get(req.tab).grant !== req.grant) await reset(req.tab);
    },
    async resetGrant(grant) {
      await Promise.all([...held].filter(([, e]) => e.grant === grant).map(([tabId]) => reset(tabId)));
    },
    async resetAll() { await Promise.all([...held.keys()].map(reset)); },
    // A worker start holds no attachment of its own: detach any its previous run left.
    async sweep() {
      if (typeof chrome.debugger?.getTargets !== "function") return;
      const targets = await chrome.debugger.getTargets().catch(() => []);
      await Promise.all(targets.filter(t => t.attached && Number.isInteger(t.tabId))
        .map(t => chrome.debugger.detach({tabId:t.tabId}).catch(() => {})));
    },
    // act() shares a held attachment; it detaches only one it attached itself (returns true).
    async attach(debuggee) {
      if (held.has(debuggee.tabId)) return false;
      await chrome.debugger.attach(debuggee, "1.3");
      return true;
    },
  };
})();
