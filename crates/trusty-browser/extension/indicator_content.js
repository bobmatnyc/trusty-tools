// Keep the indicator in the extension's isolated world and outside the site's stylesheet.
(() => {
  if (globalThis.trustyIndicatorInstalled) return;
  globalThis.trustyIndicatorInstalled = true;
  let active = null;
  function clear() {
    if (!active) return;
    clearTimeout(active.timer);
    active.element.remove();
    active = null;
  }
  chrome.runtime.onMessage.addListener((message, sender, reply) => {
    if (sender.id !== chrome.runtime.id || window !== top || message.type !== "trusty-control-indicator") return;
    if (message.action === "clear") {
      if (active?.claim === message.claim) clear();
      reply({shown:false});
      return;
    }
    if (message.action !== "show" || !message.claim || !message.workflow ||
        !Number.isFinite(message.ttl_ms) || message.ttl_ms <= 0) return;
    clear();
    const element = document.createElement("div");
    element.setAttribute("role", "status");
    element.setAttribute("aria-label", "Trusty Browser is controlling this tab · " + message.workflow);
    element.style.cssText = `
      all: initial !important;
      position: fixed !important;
      inset: 0 !important;
      display: block !important;
      box-sizing: border-box !important;
      border: 3px solid #B7410E !important;
      z-index: 2147483647 !important;
      pointer-events: none !important;
    `;
    document.documentElement.append(element);
    active = {claim:message.claim, element, timer:setTimeout(clear, message.ttl_ms)};
    reply({shown:element.isConnected});
  });
})();
