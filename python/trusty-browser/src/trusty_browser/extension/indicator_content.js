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
    const shadow = element.attachShadow({mode:"closed"});
    const style = document.createElement("style");
    style.textContent = `div{display:flex;align-items:center;gap:9px;padding:8px 13px;border:2px solid #edb789;
      border-radius:7px;background:#2c1e13;color:#fff5e9;box-shadow:0 2px 8px #0006;
      font:600 13px/1.35 system-ui,sans-serif;max-width:min(88vw,680px)}
      i{border:2px solid #ffbd89;background:#dc773e;width:9px;height:9px;border-radius:50%;flex:none}
      span{overflow-wrap:anywhere}`;
    const box = document.createElement("div");
    element.setAttribute("role","status");
    const dot = document.createElement("i");
    dot.setAttribute("aria-hidden","true");
    const label = document.createElement("span");
    label.textContent = "Trusty Browser is controlling this tab · " + message.workflow;
    element.setAttribute("aria-label", label.textContent);
    box.setAttribute("aria-hidden","true");
    box.append(dot, label);
    shadow.append(style, box);
    element.style.cssText = "all:initial!important;position:fixed!important;display:block!important;top:10px!important;right:12px!important;z-index:2147483647!important;pointer-events:none!important";
    document.documentElement.append(element);
    active = {claim:message.claim, element, timer:setTimeout(clear, message.ttl_ms)};
    reply({shown:element.isConnected});
  });
})();
