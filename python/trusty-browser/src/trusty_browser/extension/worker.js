importScripts("indicator.js", "pages.js", "codes.js");
const origin = "chrome-extension://" + chrome.runtime.id;
let connection = null;
let connecting = false;
let ready = false;
let epoch = 0;
let retryMs = 250;
let timer;
// #11239: Connect waits here for the native port's acknowledgement after a fresh pairing.
let readyWaiters = [];

const storageReady = chrome.storage.local.setAccessLevel({accessLevel:"TRUSTED_CONTEXTS"});
// #11363: a self-reload clears storage.session; a one-shot storage.local handoff, written just before
// chrome.runtime.reload() and consumed here, keeps the browser incarnation so the grant survives a
// republish. A real browser restart finds no fresh handoff and gets a new incarnation.
const HANDOFF_MS = 60000;
const lifetime = (async () => {
  const {incarnation} = await chrome.storage.session.get("incarnation");
  if (incarnation) return incarnation;
  await storageReady;
  const {reload_incarnation:handoff} = await chrome.storage.local.get("reload_incarnation");
  if (handoff !== undefined) await chrome.storage.local.remove("reload_incarnation");
  const carried = typeof handoff?.incarnation === "string" && handoff.incarnation &&
    Number.isFinite(handoff.at) && Date.now() - handoff.at >= 0 && Date.now() - handoff.at < HANDOFF_MS;
  const fresh = carried ? handoff.incarnation : crypto.randomUUID();
  await chrome.storage.session.set({incarnation:fresh});
  return fresh;
})();
const report = status => chrome.storage.session.set({status});
async function identity() {
  const {email} = await chrome.identity.getProfileUserInfo({accountStatus:"ANY"});
  if (!email) throw new Error("No signed-in Chrome account");
  return email.trim().toLowerCase();
}

const bytes = new TextEncoder();
const hex = buffer => Array.from(new Uint8Array(buffer), x => x.toString(16).padStart(2, "0")).join("");
const isHex = value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
function transcript(role, hello, nonce) {
  return bytes.encode(JSON.stringify(["trusty-browser-native-v1",role,origin,
    hello.profile,hello.email,hello.incarnation,hello.nonce,nonce]));
}
function settleReady(ok) {
  ready = ok;
  if (!ok) return;
  for (const resolve of readyWaiters.splice(0)) resolve();
}
// Resolves once the broker acknowledged this worker's hello; refuses after `ms` without one.
function online(ms = 5000) {
  if (ready) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const wait = setTimeout(() => {
      readyWaiters = readyWaiters.filter(item => item !== done);
      reject(new Error("BrowserDisconnected"));
    }, ms);
    const done = () => { clearTimeout(wait); resolve(); };
    readyWaiters.push(done);
  });
}
// #11363: the asset stamp this worker runs, written into the manifest by `trusty-browser extension-path`.
function loadedAssets() {
  const name = chrome.runtime.getManifest?.()?.version_name;
  return typeof name === "string" && name.includes("+") ? name.split("+").pop() : null;
}
// Why: Chrome keeps running the code it loaded, so republished assets need a reload (#11363).
// What: when the broker reports another stamp, reloads the extension at most once per stamp. The guard is
//   in storage.local, because the reload clears storage.session; the incarnation handoff is written last.
// Test: tests/test_worker.mjs `a stale asset stamp in the acknowledgement reloads the extension once`,
//   `a self-reload keeps the browser incarnation and a browser restart does not`.
async function checkAssets(stamp) {
  if (typeof stamp !== "string" || !stamp || stamp === loadedAssets()) return;
  const {reloaded_for:done} = await chrome.storage.local.get("reloaded_for");
  if (done === stamp) return;
  await chrome.storage.local.set({reloaded_for:stamp});
  await chrome.storage.local.set({reload_incarnation:{incarnation:await lifetime, at:Date.now()}});
  chrome.runtime.reload();
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
    // #11239: a pairing credential says hello exactly as an enrollment one does.
    const {credential:enrollment} = await codes.stored();
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
    // Grants recorded after this hello survive the acknowledgement's prune.
    const fresh = codes.watchMints();
    const lost = () => { settleReady(false); codes.unwatch(fresh); void codes.offline.lost(); };
    function drop() {
      if (connection !== port) return;
      connection = null;
      void TrustyControlIndicator.clearAll();
      lost();
      clearTimeout(timeout);
      port.disconnect();
      schedule();
    }
    port.onDisconnect.addListener(() => {
      void chrome.runtime.lastError;
      if (connection === port) {
        connection = null;
        void TrustyControlIndicator.clearAll();
        lost();
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
          if (msg.ok !== true || typeof msg.generation !== "string" || !msg.generation ||
              !Array.isArray(msg.live) || !msg.live.every(g => typeof g === "string")) throw new Error("Pairing refused");
          phase = "active";
          retryMs = 250;
          clearTimeout(timeout);
          await codes.acknowledged(msg.live, fresh);
          await report("Connected as " + enrollment.profile);
          settleReady(true);
          await checkAssets(msg.assets);
          return;
        }
        if (phase !== "active" || !["string","number"].includes(typeof msg.id)) throw new Error("Unexpected message");
        let reply;
        // #11239: the workflow adapter ops (inspect, snapshot, click, lease_*) are gone with content.js.
        if (pageOps().handles(msg.op)) {
          try { reply = await pageOps().command(msg); } catch (error) { reply = pageOps().failure(msg, error); }
        } else reply = {id:msg.id, ok:false, error:"InvalidOperation"};
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
  settleReady(false);
  void TrustyControlIndicator.clearAll();
  void codes.offline.lost();
  const port = connection;
  connection = null;
  port?.disconnect();
  retryMs = 250;
  void connect();
}
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
    const timeout = setTimeout(() => finish(null,"Trusty Browser timed out. Reopen the popup to retry."),8000);
    port.onDisconnect.addListener(() => finish(null,chrome.runtime.lastError?.message || "Enrollment disconnected"));
    port.onMessage.addListener(reply => {
      if (reply?.ok === true && CODE_OPS.has(request.op)) return finish(reply,null);
      const error = new Error(typeof reply?.error === "string" ? reply.error : "Invalid enrollment reply");
      // Only the verified enrollment host can report a definitive protocol refusal.
      if (reply?.ok === false && typeof reply.error === "string") error.response = reply;
      finish(null,error);
    });
    port.postMessage(request);
  });
}

// #11239: the verified enroll channel carries pairing, Connect, the grant list and revoke; the workflow
// enroll ops are gone.
const CODE_OPS = new Set(["connect","grants","revoke_grant","revoke_grants","pair","unpair"]);
let originChain = Promise.resolve();
const origins = {
  remember(tab, value) {
    const step = originChain.then(async () => {
      const {origins:map = {}} = await chrome.storage.session.get("origins");
      await chrome.storage.session.set({origins:{...map, [tab]:value}});
    });
    originChain = step.catch(() => {});
    return step.catch(() => {});
  },
  async originOf(tab) {
    const {origins:map} = await chrome.storage.session.get("origins");
    return typeof map?.[tab] === "string" ? map[tab] : null;
  },
};
let pageCommands;
const pageOps = () => pageCommands ||= TrustyPages.commands({identity, ended:(grant, last) => codes.ended(grant, last),
  grant:async grant => (await codes.readTable())[grant] || null, ...origins});
const codes = TrustyCodes({bootstrap, identity, online, reconnect:() => reconnect()});
const codeTypes = ["status","connect","pair","unpair","grants","revoke_grant","revoke_grants"];
chrome.runtime.onMessage.addListener((msg,sender,reply) => {
  if (sender.id !== chrome.runtime.id || sender.url !== origin + "/setup.html" || !codeTypes.includes(msg?.type)) return;
  // Owner ruling Dqw 2026-11-16: the broker's reason code reaches the popup.
  const reason = error => /^[a-z_]{1,40}$/.test(error.response?.reason ?? "") ? {reason:error.response.reason} : {};
  storageReady.then(() => codes.setup(msg)).then(reply,error => reply({ok:false,error:error.message,...reason(error)}));
  return true;
});
// #11239: a profile that used the removed site enrollment holds registered scripts naming content.js.
void (async () => {
  try {
    const ids = (await chrome.scripting.getRegisteredContentScripts()).map(s => s.id).filter(i => i.startsWith("site-"));
    if (ids.length) await chrome.scripting.unregisterContentScripts({ids});
  } catch { /* Nothing registered, or no scripting API. */ }
})();
chrome.identity.onSignInChanged.addListener(reconnect);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
chrome.alarms.onAlarm.addListener(alarm => {
  if (alarm.name === "reconnect") { void connect(); void codes.tick().catch(() => {}); }
});
chrome.alarms.create("reconnect", {periodInMinutes:1});
// A (re)started worker is offline until the broker acknowledges it; the acknowledgement prunes the table.
void codes.offline.lost();
void connect();
