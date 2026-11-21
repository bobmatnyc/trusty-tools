// The global badge is private to this Chrome profile. Each border is pinned to one document.
globalThis.TrustyControlIndicator = (() => {
  const claims = new Map();
  let pending = Promise.resolve();
  let codeGrants = 0;
  function serialized(task) {
    const result = pending.then(task);
    pending = result.catch(() => {});
    return result;
  }
  async function badge() {
    const names = [...new Set([...claims.values()].map(item => item.workflow))];
    const on = names.length > 0 || codeGrants > 0;
    if (on) {
      await chrome.action.setBadgeBackgroundColor({color:"#963800"});
      await chrome.action.setBadgeTextColor({color:"#FFFFFF"});
    }
    await chrome.action.setBadgeText({text:on ? "ON" : ""});
    const granted = codeGrants ? "agent access granted" : "";
    await chrome.action.setTitle({title:names.length
      ? "Trusty Browser controls " + names.join(", ") + (granted && "; " + granted)
      : granted ? "Trusty Browser: " + granted : "Trusty Browser"});
  }
  // #11239: activation-code grants show the badge while the worker's grant table is not empty.
  function grants(count) { return serialized(async () => { codeGrants = count; await badge(); }); }
  async function remove(tabId, claim) {
    const item = claims.get(tabId);
    if (!item || (claim && item.claim !== claim)) return;
    claims.delete(tabId);
    clearTimeout(item.timer);
    try {
      await chrome.tabs.sendMessage(tabId, {type:"trusty-control-indicator",action:"clear",claim:item.claim},
        {documentId:item.documentId});
    } catch { /* A closed tab or navigated document already removed its border. */ }
    await badge();
  }
  function update(tabId, documentId, state) {
    return serialized(async () => {
      if (!Number.isFinite(state.ttl_ms) || state.ttl_ms <= 0 || !state.workflow || !state.claim || !documentId)
        throw new Error("InvalidIndicator");
      const previous = claims.get(tabId);
      if (previous && (previous.documentId !== documentId || previous.claim !== state.claim))
        await remove(tabId, previous.claim);
      const ack = await chrome.tabs.sendMessage(tabId, {type:"trusty-control-indicator", action:"show",
        claim:state.claim,workflow:state.workflow,ttl_ms:state.ttl_ms}, {documentId});
      if (!ack?.shown) throw new Error("IndicatorUnavailable");
      clearTimeout(previous?.timer);
      const timer = setTimeout(() => clear(tabId, state.claim).catch(() => {}), state.ttl_ms);
      claims.set(tabId, {documentId, claim:state.claim, workflow:state.workflow, timer});
      try { await badge(); } catch (error) { await remove(tabId,state.claim); throw error; }
    });
  }
  function clear(tabId, claim) { return serialized(() => remove(tabId,claim)); }
  function clearAll() {
    return serialized(async () => {
      for (const tabId of [...claims.keys()]) await remove(tabId);
      await badge();
    });
  }
  return {update, clear, clearAll, grants};
})();
