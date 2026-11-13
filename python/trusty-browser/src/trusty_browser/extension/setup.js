const $ = id => document.getElementById(id);
const activate = $("activate");
const statusLine = $("status");
const robot = $("robot");
const stage = $("stage");
const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");
document.addEventListener("pointermove", event => {
  if (event.pointerType === "touch" || reduceMotion.matches) return;
  const rect = stage.getBoundingClientRect();
  const direction = (point, start, size) => Math.max(-1, Math.min(1, 2 * (point - start) / size - 1));
  const x = direction(event.clientX, rect.left, rect.width);
  const y = direction(event.clientY, rect.top, rect.height);
  robot.style.transform = `translate(${x * 7}px, ${y * 4}px) rotateY(${x * 19}deg) rotateX(${-y * 12}deg)`;
});
const centerRobot = () => robot.style.removeProperty("transform");
document.documentElement.addEventListener("pointerleave", centerRobot);
window.addEventListener("blur", centerRobot);
reduceMotion.addEventListener("change", centerRobot);

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
refresh().catch(error => { statusLine.textContent = errorMessage(error); })
  .finally(() => { activate.disabled = false; });
