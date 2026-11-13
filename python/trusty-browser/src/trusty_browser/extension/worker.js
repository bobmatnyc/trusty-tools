importScripts("indicator.js");
const origin = "chrome-extension://" + chrome.runtime.id;
const documents = new Map();
let connection = null;
let connecting = false;
let epoch = 0;
let retryMs = 250;
let timer;

const storageReady = chrome.storage.local.setAccessLevel({accessLevel:"TRUSTED_CONTEXTS"});
const lifetime = (async () => {
  const {incarnation} = await chrome.storage.session.get("incarnation");
  if (incarnation) return incarnation;
  const fresh = crypto.randomUUID();
  await chrome.storage.session.set({incarnation:fresh});
  return fresh;
})();
const report = status => chrome.storage.session.set({status});
async function identity() {
  const {email} = await chrome.identity.getProfileUserInfo({accountStatus:"ANY"});
  if (!email) throw new Error("No signed-in Chrome account");
  return email.trim().toLowerCase();
}

function allowedDocument(url) {
  if (!url) return false;
  const address = new URL(url);
  return address.protocol === "https:" ||
    (address.protocol === "http:" && address.hostname === "127.0.0.1");
}

chrome.runtime.onConnect.addListener(port => {
  const s = port.sender;
  if (port.name !== "qa-document" || s?.id !== chrome.runtime.id || s.frameId !== 0 ||
      !Number.isInteger(s.tab?.id) || !s.documentId || !allowedDocument(s.url)) {
    port.disconnect();
    return;
  }
  const doc = {id:s.documentId, url:s.url};
  if (documents.has(s.tab.id)) void TrustyControlIndicator.clear(s.tab.id);
  documents.set(s.tab.id, doc);
  port.onDisconnect.addListener(() => {
    if (documents.get(s.tab.id) === doc) {
      documents.delete(s.tab.id);
      void TrustyControlIndicator.clear(s.tab.id);
    }
  });
});

async function dispatch(req, valid) {
  if (!["inspect","snapshot","click","lease_clear","lease_show"].includes(req.op)) throw new Error("InvalidOperation");
  if (await identity() !== req.email) throw new Error("ProfileMismatch");
  let tabId = req.tab;
  if (req.op === "lease_clear") {
    if (!Number.isInteger(tabId) || typeof req.lease !== "string" || !req.lease || !valid()) {
      throw new Error("InvalidLease");
    }
    await TrustyControlIndicator.clear(tabId,req.lease);
    return {id:req.id,ok:true,email:req.email,url:req.url,account:req.account,tab:tabId,result:"cleared"};
  }
  if (req.op === "inspect") {
    const matches = [...documents].filter(([, d]) => d.url === req.url);
    if (matches.length !== 1) throw new Error(matches.length ? "AmbiguousTab" : "TabNotFound");
    tabId = matches[0][0];
  }
  if (!Number.isInteger(tabId)) throw new Error("InvalidTab");
  const doc = documents.get(tabId);
  if (!doc || doc.url !== req.url) throw new Error("TabNotFound");
  const tab = await chrome.tabs.get(tabId);
  const email = await identity();
  if (email !== req.email) throw new Error("ProfileMismatch");
  if (!valid() || tab.id !== tabId || doc !== documents.get(tabId)) throw new Error("StaleTab");
  const msg = await chrome.tabs.sendMessage(tabId, {
    op:req.op === "lease_show" ? "snapshot" : req.op, url:req.url, account:req.account, marker:req.marker
  }, {documentId:doc.id});
  if (!msg?.ok) throw new Error(msg?.error || "DocumentUnavailable");
  if (await identity() !== email || !valid()) throw new Error("ProfileMismatch");
  if (req.op === "lease_show") {
    if (typeof req.lease !== "string" || !req.lease || typeof req.workflow !== "string" || !req.workflow ||
        !Number.isFinite(req.ttl_ms) || req.ttl_ms <= 0) throw new Error("InvalidLease");
    await TrustyControlIndicator.update(tabId,doc.id,{claim:req.lease,workflow:req.workflow,ttl_ms:req.ttl_ms});
    if (!valid() || documents.get(tabId) !== doc) {
      await TrustyControlIndicator.clear(tabId,req.lease);
      throw new Error("StaleTab");
    }
  }
  return {id:req.id, ok:true, email, url:msg.url, account:msg.account, tab:tabId, result:msg.result};
}

const bytes = new TextEncoder();
const hex = buffer => Array.from(new Uint8Array(buffer), x => x.toString(16).padStart(2, "0")).join("");
const isHex = value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
function transcript(role, hello, nonce) {
  return bytes.encode(JSON.stringify(["trusty-browser-native-v1",role,origin,
    hello.profile,hello.email,hello.incarnation,hello.nonce,nonce]));
}
function schedule() {
  clearTimeout(timer);
  timer = setTimeout(connect, retryMs);
  retryMs = Math.min(30000, 2 * retryMs);
}
async function connect() {
  if (connecting || connection) return;
  connecting = true;
  const generation = epoch;
  try {
    await storageReady;
    const {enrollment} = await chrome.storage.local.get("enrollment");
    if (!enrollment) return;
    const email = await identity();
    const hello = {type:"hello", profile:enrollment.profile, email, incarnation:await lifetime,
      nonce:hex(crypto.getRandomValues(new Uint8Array(32)))};
    const key = await crypto.subtle.importKey("raw",bytes.encode(enrollment.key),
      {name:"HMAC",hash:"SHA-256"},false,["verify","sign"]);
    if (generation !== epoch) return;
    const port = chrome.runtime.connectNative("com.trusty.browser");
    connection = port;
    const valid = () => connection === port && epoch === generation;
    let phase = "challenge";
    const timeout = setTimeout(() => drop(), 5000);
    function drop() {
      if (connection !== port) return;
      connection = null;
      void TrustyControlIndicator.clearAll();
      clearTimeout(timeout);
      port.disconnect();
      schedule();
    }
    port.onDisconnect.addListener(() => {
      void chrome.runtime.lastError;
      if (connection === port) {
        connection = null;
        void TrustyControlIndicator.clearAll();
        clearTimeout(timeout);
        void report("Disconnected. Retrying…");
        schedule();
      }
    });
    port.onMessage.addListener(async msg => {
      try {
        if (!valid()) return;
        if (phase === "challenge") {
          phase = "checking";
          if (msg.type !== "challenge" || !isHex(msg.proof) || !isHex(msg.nonce)) throw new Error("Pairing refused");
          const signature = Uint8Array.from(msg.proof.match(/../g), byte => parseInt(byte, 16));
          if (!await crypto.subtle.verify("HMAC",key,signature,transcript("broker",hello,msg.nonce))) {
            throw new Error("Broker proof failed");
          }
          const proof = hex(await crypto.subtle.sign("HMAC",key,transcript("extension",hello,msg.nonce)));
          if (!valid()) return;
          phase = "ready";
          port.postMessage({type:"proof",proof});
          return;
        }
        if (phase === "ready") {
          if (msg.ok !== true || typeof msg.generation !== "string" || !msg.generation) throw new Error("Pairing refused");
          phase = "active";
          retryMs = 250;
          clearTimeout(timeout);
          await report("Connected as " + enrollment.profile);
          return;
        }
        if (phase !== "active" || !["string","number"].includes(typeof msg.id)) throw new Error("Unexpected message");
        let reply;
        try { reply = await dispatch(msg, valid); }
        catch (error) { reply = {id:msg.id, ok:false, error:error.message}; }
        if (valid()) port.postMessage(reply);
      } catch (error) {
        if (valid()) { void report(error.message); drop(); }
      }
    });
    port.postMessage(hello);
  } catch (error) {
    void report(error.message);
    schedule();
  } finally {
    connecting = false;
    if (generation !== epoch) schedule();
  }
}

function reconnect() {
  epoch++;
  void TrustyControlIndicator.clearAll();
  const port = connection;
  connection = null;
  port?.disconnect();
  retryMs = 250;
  void connect();
}
async function enableSite(pattern) {
  const [tab] = await chrome.tabs.query({active:true,lastFocusedWindow:true});
  if (!tab?.url || new URL(tab.url).protocol !== "https:" ||
      pattern !== new URL(tab.url).protocol + "//" + new URL(tab.url).hostname + "/*" ||
      !await chrome.permissions.contains({origins:[pattern]})) {
    throw new Error("Access to this site has not been granted.");
  }
  const id = "site-" + hex(await crypto.subtle.digest("SHA-256",bytes.encode(pattern)));
  if (!(await chrome.scripting.getRegisteredContentScripts({ids:[id]})).length) {
    await chrome.scripting.registerContentScripts([{id,matches:[pattern],js:["indicator_content.js","content.js"],runAt:"document_end"}]);
  }
  if (!documents.has(tab.id)) {
    await chrome.scripting.executeScript({target:{tabId:tab.id,frameIds:[0]},files:["indicator_content.js","content.js"]});
  }
  return {ok:true};
}
async function activeRoute() {
  const [tab] = await chrome.tabs.query({active:true,lastFocusedWindow:true});
  const doc = documents.get(tab?.id);
  if (!doc) throw new Error("Open a supported signed-in site in the active tab.");
  const email = await identity();
  const reply = await chrome.tabs.sendMessage(tab.id,{op:"preview",url:doc.url},{documentId:doc.id});
  if (!reply?.ok) throw new Error(reply?.error || "Could not verify site account");
  const [active] = await chrome.tabs.query({active:true,lastFocusedWindow:true});
  if (email !== await identity() || active?.id !== tab.id || doc !== documents.get(tab.id) ||
      reply.url !== doc.url || typeof reply.account !== "string" || !reply.account) {
    throw new Error("The profile, tab or site changed. Reopen the popup.");
  }
  return {tab:tab.id,document:doc.id,url:doc.url,email,account:reply.account};
}
const sameRoute = (a,b) => a && b && ["email","url","account"].every(k => a[k] === b[k]);
function bootstrap(request) {
  return new Promise((resolve,reject) => {
    const port = chrome.runtime.connectNative("com.trusty.browser.enroll");
    let settled = false;
    const finish = (reply,error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      port.disconnect();
      if (error) reject(error instanceof Error ? error : new Error(error)); else resolve(reply);
    };
    const timeout = setTimeout(() => finish(null,"Enrollment timed out. Reopen the popup to retry."),8000);
    port.onDisconnect.addListener(() => finish(null,chrome.runtime.lastError?.message || "Enrollment disconnected"));
    port.onMessage.addListener(reply => {
      if (reply?.ok === true && reply.workflow === request.workflow) return finish(reply,null);
      const error = new Error(typeof reply?.error === "string" ? reply.error : "Invalid enrollment reply");
      // Only the verified enrollment host can report a definitive protocol refusal.
      if (reply?.ok === false && typeof reply.error === "string") error.response = reply;
      finish(null,error);
    });
    port.postMessage(request);
  });
}
let enrollmentBusy = false;
let replacement = null;
const definiteRefusals = new Set(["WorkflowCredentialOccupied","InvalidEnrollment","InvalidRequest","Unauthorized","EnrollmentConflict",
  "ReplacementStale","EnrollmentRevoked","NotTrusted","UnsupportedOperation"]);
function approvedReplacement(msg,route) {
  return replacement && replacement.id === msg.confirmation && replacement.workflow === msg.workflow &&
    replacement.tab === route.tab && replacement.document === route.document && sameRoute(replacement,route);
}
function usablePrevious(previous) {
  return isHex(previous?.version) && ["email","url","account"].every(k =>
    typeof previous[k] === "string" && previous[k].length > 0 && previous[k].length <= 8192);
}
async function setup(msg) {
  await storageReady;
  if (msg.type === "enable") return await enableSite(msg.pattern);
  if (msg.type === "cancel") {
    if (replacement?.id === msg.confirmation) replacement = null;
    return {ok:true};
  }
  const {enrollment,pending} = await chrome.storage.local.get(["enrollment","pending"]);
  if (msg.type === "preview" && enrollment) {
    const {email,url,account} = enrollment;
    if (await identity() !== email) throw new Error("The enrolled profile changed.");
    const status = await bootstrap({op:"status",...enrollment});
    if (status.state === "active") return {ok:true,enrolled:enrollment.workflow,route:{email,url,account}};
    if (status.state !== "revoked") throw new Error("Could not verify enrollment status.");
    await chrome.storage.local.remove(["enrollment","pending"]);
    reconnect();
  }
  if (msg.type === "unenroll") {
    if (!enrollment || enrollment.workflow !== msg.workflow || enrollment.email !== await identity()) {
      throw new Error("The enrolled profile changed.");
    }
    if (enrollmentBusy) throw new Error("Enrollment already in progress.");
    enrollmentBusy = true;
    try {
      const {profile,key,email,workflow,url,account} = enrollment;
      await bootstrap({op:"unenroll",profile,key,email,workflow,url,account});
      await chrome.storage.local.remove(["enrollment","pending"]);
      reconnect();
      return {ok:true,workflow};
    } finally { enrollmentBusy = false; }
  }
  if (msg.type === "preview") {
    const [tab] = await chrome.tabs.query({active:true,lastFocusedWindow:true});
    if (!documents.has(tab?.id) && tab?.url && new URL(tab.url).protocol === "https:") {
      const url = new URL(tab.url);
      return {ok:true,permissionPattern:url.protocol + "//" + url.hostname + "/*"};
    }
  }
  const route = await activeRoute();
  if (msg.type === "preview") return {ok:true,route,pending:pending?.workflow};
  if (enrollment) throw new Error("This profile already has an enrollment.");
  if (enrollmentBusy) throw new Error("Enrollment already in progress.");
  if (!sameRoute(route,msg.route) || msg.route.tab !== route.tab || msg.route.document !== route.document) {
    throw new Error("The profile, tab or site changed. Reopen the popup.");
  }
  const workflow = msg.workflow;
  if (typeof workflow !== "string" || !/^[a-z0-9][a-z0-9_-]{0,63}$/.test(workflow)) {
    throw new Error("Use 1–64 lowercase letters, digits, underscores or hyphens, starting with a letter or digit.");
  }
  if (msg.type === "replace" && !approvedReplacement(msg,route)) {
    throw new Error("The confirmation expired. Check the workflow again.");
  }
  if (pending && (!sameRoute(pending,route) || pending.workflow !== workflow)) {
    throw new Error("A pending enrollment must be retried using its original workflow and site.");
  }
  enrollmentBusy = true;
  try {
    const data = pending ? {...pending} : {profile:crypto.randomUUID(),key:hex(crypto.getRandomValues(new Uint8Array(32))),
      email:route.email,url:route.url,account:route.account,workflow};
    // An interrupted confirmed replacement must retry its original compare-and-swap.
    const version = pending?.previous_version || (msg.type === "replace" ? replacement.version : null);
    delete data.previous_version;
    await chrome.storage.local.set({pending:version ? {...data,previous_version:version} : data});
    replacement = null;
    try {
      await bootstrap(version ? {op:"replace",...data,previous_version:version} : {op:"enroll",...data});
    } catch (error) {
      const response = error.response;
      if (response && definiteRefusals.has(response.error)) await chrome.storage.local.remove("pending");
      if (response?.error === "EnrollmentConflict" && usablePrevious(response.previous)) {
        const {email,url,account,version} = response.previous;
        replacement = {...route,workflow,version,id:crypto.randomUUID()};
        return {ok:true,previous:{email,url,account},confirmation:replacement.id};
      }
      throw error;
    }
    await chrome.storage.local.set({enrollment:data});
    await chrome.storage.local.remove("pending");
    reconnect();
    return {ok:true,workflow};
  } finally { enrollmentBusy = false; }
}
chrome.runtime.onMessage.addListener((msg,sender,reply) => {
  if (sender.id !== chrome.runtime.id || sender.url !== origin + "/setup.html" ||
      !["enroll","replace","cancel","preview","enable","unenroll"].includes(msg.type)) return;
  setup(msg).then(reply,error => reply({ok:false,error:error.message}));
  return true;
});
chrome.identity.onSignInChanged.addListener(reconnect);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
chrome.alarms.onAlarm.addListener(alarm => { if (alarm.name === "reconnect") void connect(); });
chrome.alarms.create("reconnect", {periodInMinutes:1});
void connect();
