const statusLine = document.querySelector("#status");
const workflow = document.querySelector("#workflow");
const form = document.querySelector("#setup");
const button = document.querySelector("#enroll");
const enable = document.querySelector("#enable");
const remove = document.querySelector("#remove");
let permissionPattern;
let enrolledWorkflow;
let route;

async function request(message) {
  const reply = await chrome.runtime.sendMessage(message);
  if (!reply?.ok) throw new Error(reply?.error || "Trusty Browser is unavailable");
  return reply;
}
async function preview() {
  const reply = await request({type:"preview"});
  if (reply.permissionPattern) {
    permissionPattern = reply.permissionPattern;
    enable.hidden = false;
    statusLine.textContent = "The site must support Trusty Browser account verification.";
    return;
  }
  enable.hidden = true;
  route = reply.route;
  document.querySelector("#profile").textContent = route.email;
  document.querySelector("#site").textContent = route.url;
  document.querySelector("#account").textContent = route.account;
  document.querySelector("#route").hidden = false;
  if (reply.enrolled) {
    enrolledWorkflow = reply.enrolled;
    remove.hidden = false;
    statusLine.textContent = "Enrolled workflow: " + reply.enrolled;
    return;
  }
  workflow.value = reply.pending || "";
  form.hidden = false;
  statusLine.textContent = "";
  workflow.focus();
}
const transfer = document.querySelector("#transfer");
const cancel = document.querySelector("#cancel");
const confirm = document.querySelector("#replace");
let confirmation;
async function submit(type) {
  button.disabled = confirm.disabled = true;
  statusLine.textContent = "";
  try {
    const reply = await request({type,workflow:workflow.value.trim(),route,confirmation});
    form.hidden = true;
    if (reply.previous) {
      confirmation = reply.confirmation;
      const old = reply.previous;
      document.querySelector("#transfer-route").textContent =
        "Move " + workflow.value + " from " + old.email + " (" + old.account + " at " + old.url +
        ") to " + route.email + " (" + route.account + " at " + route.url + ")?";
      transfer.hidden = false;
      cancel.focus();
      return;
    }
    transfer.hidden = true;
    enrolledWorkflow = reply.workflow;
    remove.hidden = false;
    statusLine.textContent = "Enrolled workflow: " + reply.workflow;
  } catch (error) {
    transfer.hidden = true;
    form.hidden = false;
    statusLine.textContent = error.message;
  } finally { button.disabled = confirm.disabled = false; }
}
form.addEventListener("submit",event => { event.preventDefault(); void submit("enroll"); });
confirm.addEventListener("click",() => submit("replace"));
cancel.addEventListener("click",async () => {
  await request({type:"cancel",confirmation});
  confirmation = null;
  transfer.hidden = true;
  form.hidden = false;
  workflow.focus();
});
enable.addEventListener("click", async () => {
  try {
    if (!await chrome.permissions.request({origins:[permissionPattern]})) throw new Error("Site access declined");
    await request({type:"enable", pattern:permissionPattern});
    await preview();
  } catch (error) { statusLine.textContent = error.message; }
});
remove.addEventListener("click", async () => {
  if (remove.dataset.confirm !== "yes") {
    remove.dataset.confirm = "yes";
    remove.textContent = "Confirm removal";
    statusLine.textContent = "Remove workflow " + enrolledWorkflow + " from this profile?";
    return;
  }
  remove.disabled = true;
  try {
    await request({type:"unenroll",workflow:enrolledWorkflow});
    remove.hidden = true;
    statusLine.textContent = "Workflow removed.";
  } catch (error) { statusLine.textContent = error.message; }
  finally { remove.disabled = false; }
});
preview().catch(error => { statusLine.textContent = error.message; });
