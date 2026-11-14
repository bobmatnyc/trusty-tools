const $ = id => document.getElementById(id);
const activate = $("activate");
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

let preview;
let confirmation;
let workflow;
async function request(message) {
  const reply = await chrome.runtime.sendMessage(message);
  if (!reply?.ok) throw new Error(reply?.error || "Could not reach Trusty Browser.");
  return reply;
}
function active(name, route) {
  workflow = name;
  activate.hidden = true;
  $("details").hidden = false;
  $("profile").textContent = route.email;
  $("site").textContent = route.url;
  $("account").textContent = route.account;
  $("workflow").textContent = name;
  statusLine.textContent = "Extension active";
  void loadAccess();
}
async function refresh() {
  preview = await request({type:"preview"});
  if (preview.enrolled) active(preview.enrolled, preview.route);
}
function errorMessage(error) {
  if (/native messaging host|enrollment disconnected/i.test(error.message)) {
    return "Trusty Browser could not reach its local service. Start the service and try again.";
  }
  return error.message;
}
async function submit(replace = false) {
  activate.disabled = $("replace").disabled = $("cancel").disabled = true;
  activate.textContent = "Activating…";
  statusLine.textContent = "";
  try {
    if (!preview) await refresh();
    if (preview.enrolled) return;
    if (preview.permissionPattern) {
      const target = preview;
      if (!await chrome.permissions.request({origins:[target.permissionPattern]})) {
        throw new Error("Allow access to this site to activate Trusty Browser.");
      }
      await request({type:"enable", pattern:target.permissionPattern, tab:target.tab, url:target.url});
      await refresh();
      if (preview.enrolled) return;
      if (!preview.route || preview.route.tab !== target.tab || preview.route.url !== target.url) {
        throw new Error("The active tab changed. Reopen the popup.");
      }
    }
    const reply = await request(replace
      ? {type:"replace", workflow, route:preview.route, confirmation}
      : {type:"activate", route:preview.route});
    workflow = reply.workflow;
    if (reply.previous) {
      confirmation = reply.confirmation;
      const describe = r => r.email + " (" + r.account + " at " + r.url + ")";
      $("transfer-route").textContent = "Move this workflow from " + describe(reply.previous) +
        " to " + describe(preview.route) + "?";
      activate.hidden = true;
      $("transfer").hidden = false;
      $("cancel").disabled = false;
      $("cancel").focus();
    } else {
      $("transfer").hidden = true;
      active(workflow, preview.route);
    }
  } catch (error) {
    preview = null;
    $("transfer").hidden = true;
    activate.hidden = false;
    statusLine.textContent = errorMessage(error);
    // Prepare a fresh target while the user's next permission gesture is still available.
    try { await refresh(); } catch { /* Preserve the activation error for retry. */ }
  } finally {
    activate.disabled = $("replace").disabled = $("cancel").disabled = false;
    activate.textContent = "Activate Extension";
  }
}
activate.addEventListener("click", () => submit());
$("replace").addEventListener("click", () => submit(true));
$("cancel").addEventListener("click", async () => {
  try {
    await request({type:"cancel", confirmation});
    confirmation = null;
    $("transfer").hidden = true;
    activate.hidden = false;
    activate.focus();
  } catch (error) { statusLine.textContent = errorMessage(error); }
});
const remove = $("remove");
remove.addEventListener("click", async () => {
  if (remove.dataset.confirm !== "yes") {
    remove.dataset.confirm = "yes";
    remove.textContent = "Confirm removal";
    statusLine.textContent = "Remove this workflow from this profile?";
    return;
  }
  remove.disabled = true;
  try {
    await request({type:"unenroll", workflow});
    location.reload();
  } catch (error) { statusLine.textContent = errorMessage(error); }
  finally { remove.disabled = false; }
});

// #11239: activation codes. The code lives only in this page's memory; closing the popup discards it.
// The target tab is read once, at load, and every request names it explicitly.
const targetTab = (async () => {
  const [found] = await chrome.tabs.query({active:true, currentWindow:true});
  return Number.isInteger(found?.id) && found.id >= 0 ? found.id : null;
})().catch(() => null);
let tab = null, code = null, codeGrant = null, expiresAt = 0, ticker = null, poller = null, accessShown = false;
const clock = seconds => {
  const s = Math.max(0, Math.ceil(seconds));
  return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0");
};
function show(state) {
  for (const name of ["choose","shown","expired"]) $(name).hidden = name !== state;
}
function forgetCode() {
  clearInterval(ticker);
  code = codeGrant = null;
  $("code").value = "";
}
function countdown() {
  const left = expiresAt - Date.now() / 1000;
  if (left <= 0) {
    forgetCode();
    show("expired");
    return;
  }
  $("countdown").textContent = clock(left);
}
function scopeLabel(entry) { return entry.scope === "tab" ? "This tab" : "Whole profile"; }
function renderGrants(entries) {
  const list = $("grants");
  list.replaceChildren(...entries.map(entry => {
    const row = document.createElement("li");
    const where = [entry.origin, entry.title].filter(Boolean).join(" — ");
    const state = entry.state === "code"
      ? "waiting for agent, " + clock(entry.expires_in_s) + " left"
      : "agent connected, idle " + clock(entry.idle_s);
    row.textContent = scopeLabel(entry) + (where ? ": " + where : "") + " · " + state;
    const disconnect = document.createElement("button");
    disconnect.className = "secondary";
    disconnect.textContent = "Disconnect";
    disconnect.addEventListener("click", () => revoke({type:"revoke_grant", grant:entry.grant}));
    row.append(document.createElement("br"), disconnect);
    return row;
  }));
  $("disconnect-all").hidden = entries.length === 0;
}
async function loadGrants() {
  try { renderGrants((await request({type:"grants"})).grants); }
  catch (error) { statusLine.textContent = errorMessage(error); }
}
async function revoke(message) {
  try {
    await request(message);
    if (message.type === "revoke_grants" || message.grant === codeGrant) {
      forgetCode();
      show("choose");
    }
  } catch (error) { statusLine.textContent = errorMessage(error); }
  await loadGrants();
}
function unavailable(error) {
  $("access").hidden = true;
  $("retry").hidden = false;
  statusLine.textContent = errorMessage(error);
}
async function loadAccess() {
  $("retry").hidden = true;
  try {
    tab = await targetTab;
    const preview = await request({type:"code_preview", tab});
    // A profile without a credential has no code flow until pairing ships (#11239).
    if (!preview.paired) return;
    const target = preview.target;
    $("mint-tab").disabled = !target?.access;
    $("target").textContent = target?.access
      ? [target.origin, target.title].filter(Boolean).join(" — ")
      : "Open a web page and click Trusty Browser on it";
    $("access").hidden = false;
    if (!accessShown) show("choose");
    accessShown = true;
    clearInterval(poller);
    poller = setInterval(loadGrants, 5000);
    await loadGrants();
  } catch (error) { unavailable(error); }
}
function mint(scope) {
  // The owner ruling asks for all-https access inside this click; Chrome needs the call in the gesture.
  let permission;
  try { permission = chrome.permissions.request({origins:["https://*/*"]}).catch(() => false); }
  catch { permission = Promise.resolve(false); }
  $("mint-tab").disabled = $("mint-profile").disabled = true;
  statusLine.textContent = "";
  void (async () => {
    try {
      await permission;
      const reply = await request(scope === "tab" ? {type:"mint", scope, tab} : {type:"mint", scope});
      code = reply.code;
      codeGrant = reply.grant;
      expiresAt = reply.expires_at;
      $("code").value = code;
      $("code-scope").textContent = scope === "tab" ? "This tab" : "Whole profile (every tab in this profile)";
      show("shown");
      countdown();
      clearInterval(ticker);
      ticker = setInterval(countdown, 1000);
    } catch (error) {
      statusLine.textContent = errorMessage(error);
    } finally {
      $("mint-profile").disabled = false;
      await loadAccess();
    }
  })();
}
$("mint-tab").addEventListener("click", () => mint("tab"));
$("mint-profile").addEventListener("click", () => mint("profile"));
$("copy").addEventListener("click", async () => {
  if (!code) return;
  try {
    await navigator.clipboard.writeText(code);
    statusLine.textContent = "Code copied";
  } catch { statusLine.textContent = "Could not copy the code."; }
});
$("cancel-code").addEventListener("click", () => codeGrant && revoke({type:"revoke_grant", grant:codeGrant}));
$("new-code").addEventListener("click", () => show("choose"));
$("disconnect-all").addEventListener("click", () => revoke({type:"revoke_grants"}));
$("retry").addEventListener("click", () => loadAccess());
refresh().catch(error => { statusLine.textContent = errorMessage(error); })
  .finally(() => { activate.disabled = false; });
