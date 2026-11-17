// Test harness for the dashboard-serve.py shim (scripts/test_dashboard_busy.py): runs the shim in node against a fake /api.
// Usage: node shim_harness.js <shim.js> <scenario>; prints one JSON line with what the page would have seen.
// #10896: scenario "page" runs page code over the shim against a real dashboard-serve instead of the fake /api:
// node shim_harness.js <shim.js> page <base url> <code.js>, where <code.js> defines `async function main(db)`; the harness
// prints its result as one JSON line (a thrown error prints {error, status}).
const fs = require("fs");
const [, , shimPath, scenario, pageBase, pageCode] = process.argv;
const realFetch = globalThis.fetch;
const events = [], snaps = [], t0 = Date.now();
let version = 1, putStatus = 200, getDelay = [];            // getDelay: per-GET latency queue (ms), then 20
const listeners = {};
global.window = { addEventListener: (n, f) => { listeners[n] = f; }, dispatchEvent: (e) => { events.push([Date.now() - t0, e.detail]); return true; }, location: {} };
global.CustomEvent = function (n, o) { this.type = n; this.detail = o.detail; };
global.document = { addEventListener() {}, removeEventListener() {} };
global.location = { reload() {} };
const res = (status, js) => ({ ok: status < 400, status, headers: { get: () => null }, json: async () => js });
global.fetch = async (url, o) => {
  const m = (o && o.method) || "GET";
  if (m === "GET") {
    const v = version, d = getDelay.length ? getDelay.shift() : 20;
    await new Promise((r) => setTimeout(r, d));
    return res(200, { docs: [{ id: "A01", version: v, data: { v } }], rev: "r" + v });
  }
  if (putStatus !== 200) return res(putStatus, { error: "conflict", code: "conflict" });
  version++;
  return res(200, { id: "A01", version });
};
const src = fs.readFileSync(shimPath, "utf8");
if (scenario === "page") global.fetch = (url, o) => realFetch(pageBase + url, o);
(0, eval)(src);
if (scenario === "page") {
  (0, eval)(fs.readFileSync(pageCode, "utf8"));
  (async () => {
    let out;
    try { out = await main(await window.claude.use("db")); } catch (e) { out = { error: String(e && e.message), status: e && e.status }; }
    console.log(JSON.stringify(out));
    process.exit(0);
  })();
} else (async () => {
  const db = await window.claude.use("db");
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  if (scenario === "stale") getDelay = [400];              // the first (initial) poll is slow and will return v1
  db.collection("items").onSnapshot((s) => snaps.push([Date.now() - t0, s.docs[0].version]), () => {});
  await sleep(50);                                          // the slow poll is now in flight
  if (scenario === "conflict") putStatus = 409;
  const w0 = Date.now() - t0;
  try { await db.collection("items").doc("A01").update({ x: 1 }); } catch (e) { events.push([Date.now() - t0, { thrown: e.status }]); }
  await sleep(900);
  console.log(JSON.stringify({ write_at: w0, snaps, events }));
  process.exit(0);
})();
