const $ = id => document.getElementById(id);
const statusLine = $("status");
const robot = $("robot");
const eyes = [...robot.querySelectorAll(".eye")];
const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");
document.addEventListener("pointermove", event => {
  if (event.pointerType === "touch" || reduceMotion.matches) return;
  for (const eye of eyes) {
    const rect = eye.getBoundingClientRect();
    const dx = event.clientX - rect.left - rect.width / 2;
    const dy = event.clientY - rect.top - rect.height / 2;
    const distance = Math.max(rect.width, Math.hypot(dx, dy));
    eye.style.setProperty("--look-x", (5 * dx / distance) + "%");
    eye.style.setProperty("--look-y", (5 * dy / distance) + "%");
  }
});
function centerEyes() {
  for (const eye of eyes) {
    eye.style.removeProperty("--look-x");
    eye.style.removeProperty("--look-y");
  }
}
document.documentElement.addEventListener("pointerleave", centerEyes);
window.addEventListener("blur", centerEyes);
reduceMotion.addEventListener("change", centerEyes);

// #11239 owner ruling 03:37Z: one Connect click, no code. The worker pairs the profile when needed and asks
// the broker for a standing profile grant; Disconnect revokes it at once. Contract "Popup states".
async function request(message) {
  const reply = await chrome.runtime.sendMessage(message);
  if (!reply?.ok) {
    // A missing reply means the worker predates this popup: the profile still runs an older extension.
    const error = new Error(
      reply?.error || "The Trusty Browser extension is out of date. Reload it at chrome://extensions.",
    );
    if (typeof reply?.reason === "string") error.reason = reply.reason;
    throw error;
  }
  return reply;
}
// Owner ruling Dqw 2026-11-16: a refusal shows the broker's reason code, never a bare "Unauthorized".
// Test: tests/test_activation_dom.py `test_a_refused_connect_shows_the_broker_reason_code`.
function errorMessage(error) {
  if (/native messaging host|enrollment disconnected|BrowserDisconnected/i.test(error.message)) {
    return "Trusty Browser could not reach its local service. Start the service and try again.";
  }
  return error.reason ? error.message + " (" + error.reason + ")" : error.message;
}
// Chrome needs permissions.request inside the click, so the popup tracks whether https://*/* is held.
const ALL_HTTPS = {origins:["https://*/*"]};
let httpsHeld = false;
async function checkHttps() {
  try { httpsHeld = await chrome.permissions.contains(ALL_HTTPS) === true; } catch { httpsHeld = false; }
}
const coversAllHttps = change => change?.origins?.includes("https://*/*");
try {
  chrome.permissions.onRemoved?.addListener(change => { if (coversAllHttps(change)) httpsHeld = false; });
  chrome.permissions.onAdded?.addListener(change => { if (coversAllHttps(change)) httpsHeld = true; });
} catch { /* checkHttps still refreshes the state. */ }
function askHttps() {
  try { return chrome.permissions.request(ALL_HTTPS).catch(() => false); } catch { return Promise.resolve(false); }
}
const connectButton = $("connect");
function render(state) {
  $("retry").hidden = true;
  $("offline").hidden = !!state.connected;
  $("online").hidden = !state.connected;
  $("connected-email").textContent = state.connected ? state.email : "";
  connectButton.disabled = false;
}
async function load() {
  try {
    const state = await request({type:"status"});
    await checkHttps();
    render(state);
    return state;
  } catch (error) {
    $("offline").hidden = $("online").hidden = true;
    $("retry").hidden = false;
    statusLine.textContent = errorMessage(error);
    return null;
  }
}
// Why: the ruling's single gesture must pair, get host access and create the grant (#11239).
// What: requests https://*/* inside the click when absent (it needs the gesture), then sends `connect`.
//   The connected badge shows only after the worker answered ok; any failure leaves Connect showing.
// Test: tests/test_activation_dom.py `test_connect_is_one_click_and_disconnect_revokes_at_once`,
//   `test_a_connect_whose_grant_creation_fails_does_not_show_connected`.
connectButton.addEventListener("click", () => {
  const permission = httpsHeld ? Promise.resolve(true) : askHttps();
  connectButton.disabled = true;
  statusLine.textContent = "";
  void (async () => {
    try {
      if (!await permission) throw new Error("Connect needs access to your sites. Click Connect and allow it.");
      httpsHeld = true;
      await request({type:"connect"});
      const state = await load();
      if (state?.connected) statusLine.textContent = "Connected";
    } catch (error) {
      render({connected:null});
      statusLine.textContent = errorMessage(error);
    }
  })();
});
$("disconnect").addEventListener("click", async () => {
  $("disconnect").disabled = true;
  statusLine.textContent = "";
  try {
    await request({type:"revoke_grants"});
    render({connected:null});
    statusLine.textContent = "Disconnected";
  } catch (error) { statusLine.textContent = errorMessage(error); }
  finally { $("disconnect").disabled = false; }
});
// Forget this profile: the worker's `unpair` removes the pairing too; the next Connect pairs again.
// Test: tests/test_activation_dom.py `test_forget_unpairs_and_disconnect_does_not`.
$("forget").addEventListener("click", async event => {
  event.preventDefault();
  statusLine.textContent = "";
  try {
    await request({type:"unpair"});
    render({connected:null});
    statusLine.textContent = "Forgotten";
  } catch (error) { statusLine.textContent = errorMessage(error); }
});
$("retry").addEventListener("click", () => { statusLine.textContent = ""; void load(); });
void load();
