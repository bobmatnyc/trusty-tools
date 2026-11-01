"""dashboard_page.py: the served page, its shim and the pinned reader libraries (split from dashboard-serve.py, #10161).

Why: the page was written for the claude.ai runtime (window.claude.use("db"|"user")). The server serves it unchanged
and injects a shim, so one page file runs both under claude.ai and on localhost.
What: `SHIM` defines window.claude.use over this server's JSON endpoints and emulates onSnapshot by polling every 2 s;
`inject` inserts the shared tokens.css link and then the shim right after the document's own <head> (the /live page
gets the link only). `page_rev` is sha1[:16] of the page file plus tokens.css, sent as X-Page-Rev on every response:
the shim holds the rev it was served with and reloads the tab once when it differs, after window.__stashDrafts(); it
also checks /config.json every 60 s and when the tab becomes visible, skipping while a text field holds text.
`rev_of` is a collection's snapshot rev. `vendor_file` resolves one of the fixed reader-library names: marked and
DOMPurify ship in templates/dashboard/vendor/; mermaid.min.js is not shipped and comes from the instance tree
(local/artifacts/owner-queue/vendor/) when present. Standard library only. The Rust successor is the console route
(See #10064). `STATIC_FILES` are the fixed names served at /static/<name> (the design tokens, #10571).
Test: tests/test_dashboard_store.py, tests/test_dashboard_reload.py, tests/test_dashboard_busy.py, tests/test_dashboard_doc.py
"""
import hashlib
import os
import re

import dashboard_store as ds

PAGE = os.path.join(ds.REPO, "templates", "dashboard", "index.html")
LIVE_PAGE = os.path.join(ds.REPO, "templates", "dashboard", "live.html")
# D3: the shipped reader libraries live beside the page; the instance tree keeps mermaid (not shipped) and the old copies.
VENDOR = os.path.join(ds.REPO, "templates", "dashboard", "vendor")
VENDOR_INSTANCE = os.path.join(ds.REPO, "local", "artifacts", "owner-queue", "vendor")
VENDOR_FILES = ("marked.umd.js", "purify.min.js", "mermaid.min.js")   # fixed names; nothing else under vendor/ is served
# Design system: one shared token file for every page, served at /static/<name>.
STATIC = os.path.join(ds.REPO, "templates", "dashboard")
STATIC_FILES = ("tokens.css",)   # fixed names; nothing else under templates/dashboard/ is served as static
TOKENS_CSS = os.path.join(STATIC, "tokens.css")
TOKENS = os.path.join(ds.REPO, "local", "usage", "token-hourly.jsonl")   # hourly usage rows (dashboard_viz); #10571: named here, not in the server script


def vendor_file(vendor, name):
    """The file for a /vendor/<name> request, or None: a fixed name only; the shipped dir falls back to the instance tree."""
    if name not in VENDOR_FILES:
        return None
    path = os.path.join(vendor, name)
    if not os.path.isfile(path) and vendor == VENDOR:
        path = os.path.join(VENDOR_INSTANCE, name)
    return path if os.path.isfile(path) else None


SHIM = r"""<script>
/* dashboard-serve.py shim: window.claude.use("db"|"user") over this server's /api, snapshots by polling. */
(function () {
  "use strict";
  var POLL_MS = 2000, FAIL_LIMIT = 5, listeners = new Set();
  var PAGE_REV = "__PAGE_REV__", reloading = false;
  /* The server sends the served page's hash on every API response. A different hash means index.html changed since this tab loaded it: stash unsent drafts, then reload once. */
  function checkRev(res) {
    var rev = res && res.headers && res.headers.get ? res.headers.get("X-Page-Rev") : null;
    if (!rev || !PAGE_REV || rev === PAGE_REV || reloading) return;
    reloading = true;
    try { if (window.__stashDrafts) window.__stashDrafts(); } catch (_) {}
    location.reload();
  }
  /* A tab left open with no API response (or a throttled background tab) never sees the new rev. Also check on a 60 s timer and when the tab becomes visible; skip while a text input or textarea holds text, and retry on the next tick. */
  function hasDraft() {
    return Array.prototype.some.call(document.querySelectorAll("input, textarea"), function (el) {
      var t = (el.type || "").toLowerCase();
      return (el.tagName === "TEXTAREA" || t === "" || t === "text" || t === "search" || t === "url" || t === "email" || t === "password") && String(el.value || "").trim() !== "";
    });
  }
  function revPoll() {
    if (reloading || hasDraft()) return;
    fetch("/config.json", { cache: "no-store" }).then(checkRev, function () {});
  }
  setInterval(revPoll, 60000);
  document.addEventListener("visibilitychange", function () { if (document.visibilityState === "visible") revPoll(); });
  function mkErr(status, body) {
    var e = new Error((body && body.error) || ("HTTP " + status));
    e.code = (body && body.code) || "unavailable"; e.status = status; return e;
  }
  async function api(method, path, body) {
    var res = await fetch("/api/" + path, { method: method, cache: "no-store",
      headers: body ? { "Content-Type": "application/json" } : {}, body: body ? JSON.stringify(body) : undefined });
    checkRev(res);
    var js = null; try { js = await res.json(); } catch (_) {}
    if (!res.ok) throw mkErr(res.status, js);
    return js;
  }
  /* After a write, re-fetch at once the subscriptions on the collections it touched and resolve when they have delivered. A poll already in flight started before the write and may carry the old data, and the old tick() dropped the post-write refresh while one was in flight, so the page kept the old state until the next 2 s tick (measured 2.6 s with a 0.6 s poll). The refresh now starts at once, in parallel. */
  function refreshColls(colls) {
    var jobs = [];
    listeners.forEach(function (h) { if (!colls || colls.indexOf(h.coll) >= 0) jobs.push(h.refresh()); });
    return Promise.all(jobs);
  }
  function collsOf(path, body) {
    if (path === "batch") return ((body && body.ops) || []).map(function (o) { return o.collection; });
    return [path.split("/")[0].split("?")[0]];
  }
  /* The page shows a "Processing..." overlay from these events: start, then settled (writes done and the re-fetch delivered) or error. The write's own promise still resolves as soon as the server answers. */
  function emit(phase, detail) { try { window.dispatchEvent(new CustomEvent("dash:write", { detail: Object.assign({ phase: phase }, detail || {}) })); } catch (_) {} }
  async function write(method, path, body) {
    var colls = collsOf(path, body);
    emit("start");
    try {
      var r = await api(method, path, body);
      refreshColls(colls).then(function () { emit("settled"); }, function () { emit("settled"); });
      return r;
    } catch (e) {
      refreshColls(colls).then(function () { emit("error", { status: e.status, code: e.code, message: e.message }); }, function () { emit("error", { status: e.status, code: e.code, message: e.message }); });   /* a 409 or any failure still reloads, so the page shows the current doc */
      throw e;
    }
  }
  function docSnap(id, d) { return { id: id, exists: !!d, version: d ? d.version : undefined, data: function () { return d ? d.data : undefined; } }; }
  function querySnap(res) {
    var docs = res.docs.map(function (d) { return docSnap(d.id, d); });
    return { docs: docs, size: docs.length, empty: docs.length === 0, forEach: function (f) { docs.forEach(f); } };
  }
  function subscribe(coll, load, cb, err) {
    var last = null, fails = 0, stopped = false, inflight = 0, started = 0, applied = 0;
    function tick() { return run(false); }
    /* refresh(): a fetch that starts now even when a poll is in flight (that poll began before the write and may carry old data). Results apply in start order: a slower, older fetch that lands after a newer one is dropped. */
    function refresh() { return run(true); }
    // The first load runs even in a hidden tab (else the page shows "Loading…" until shown); later polls run while hidden too (the browser throttles them), so a new decision can still beep.
    async function run(force) {
      if (stopped || (inflight && !force)) return;
      var mine = ++started; inflight++;
      try {
        var r = await load(); fails = 0;
        if (mine > applied) { applied = mine; if (r.rev !== last) { last = r.rev; if (!stopped) { try { cb(r.snap); } catch (e) { console.error(e); } } } }
      } catch (e) { fails++; if (fails === FAIL_LIMIT && err) { try { err(e); } catch (_) {} } }
      inflight--;
    }
    var h = { tick: tick, refresh: refresh, coll: coll }; listeners.add(h); run(true);
    var timer = setInterval(tick, POLL_MS);
    document.addEventListener("visibilitychange", tick);
    return function () { stopped = true; clearInterval(timer); listeners.delete(h); document.removeEventListener("visibilitychange", tick); };
  }
  function DocRef(coll, id) {
    var p = coll + "/" + encodeURIComponent(id);
    this.id = id;
    this.get = async function () { try { return docSnap(id, await api("GET", p)); } catch (e) { if (e.status === 404) return docSnap(id, null); throw e; } };
    this.set = function (data, opts) { return write("PUT", p, { data: data, if_version: opts && opts.if_version }); };
    this.update = function (patch, opts) { return write("PATCH", p, { data: patch, if_version: opts && opts.if_version }); };
    this.delete = function (opts) { return write("DELETE", p + (opts && opts.if_version != null ? "?if_version=" + opts.if_version : "")); };
    this.onSnapshot = function (cb, err) {
      return subscribe(coll, async function () {
        try { var d = await api("GET", p); return { rev: "v" + d.version, snap: docSnap(id, d) }; }
        catch (e) { if (e.status === 404) return { rev: "missing", snap: docSnap(id, null) }; throw e; }
      }, cb, err);
    };
  }
  function Query(coll, o) {
    this.doc = function (id) { return new DocRef(coll, id); };
    this.orderBy = function (f, dir) { return new Query(coll, Object.assign({}, o, { order_by: f, dir: dir === "desc" ? "desc" : "asc" })); };
    this.limit = function (n) { return new Query(coll, Object.assign({}, o, { limit: n })); };
    function qs() { var p = new URLSearchParams(o); var s = p.toString(); return coll + (s ? "?" + s : ""); }
    this.get = async function () { return querySnap(await api("GET", qs())); };
    this.add = async function (data) { var r = await write("POST", coll, { data: data }); return new DocRef(coll, r.id); };
    this.onSnapshot = function (cb, err) {
      return subscribe(coll, async function () { var r = await api("GET", qs()); return { rev: r.rev, snap: querySnap(r) }; }, cb, err);
    };
  }
  var db = {
    collection: function (name) { return new Query(name, {}); },
    doc: function (path) { var i = path.indexOf("/"); return new DocRef(path.slice(0, i), path.slice(i + 1)); },
    batch: async function (ops) { return (await write("POST", "batch", { ops: ops })).results; }   /* all-or-nothing; each op may carry if_version */
  };
  async function user() {
    var u = await api("GET", "user");
    return { id: u.id, name: u.name, can: async function () { return u.can_write !== false; } };
  }
  window.claude = { use: async function (cap) { return cap === "db" ? db : cap === "user" ? await user() : null; } };
})();
</script>
"""


_REV_CACHE = {}


def _file_stamp(path):
    st = os.stat(path)
    return (st.st_mtime_ns, st.st_size)


def page_rev(path, extra=()):
    """First 16 hex of the sha1 of the page file plus each file in `extra` (tokens.css), so a token-only change
    reloads open tabs; re-read only when a file's (mtime, size) changes. A missing extra file raises OSError like the page."""
    paths = (path,) + tuple(extra)
    key = tuple(_file_stamp(p) for p in paths)
    hit = _REV_CACHE.get(paths)
    if hit and hit[0] == key:
        return hit[1]
    h = hashlib.sha1()
    for p in paths:
        with open(p, "rb") as f:
            h.update(f.read())
    rev = h.hexdigest()[:16]
    _REV_CACHE[paths] = (key, rev)
    return rev


_HEAD_OPEN = re.compile(r"\s*(?:<!doctype[^>]*>\s*)?(?:<html(?:\s[^>]*)?>\s*)?<head(?:\s[^>]*)?>", re.I)


def tokens_link(rev=""):
    return '<link rel="stylesheet" href="/static/tokens.css%s">' % ("?v=" + rev if rev else "")


def inject(html, rev="", shim_on=True):
    """Insert the tokens.css link, then the shim, right after <head> (before any page script); prepend when there is no head.
    shim_on=False (the /live page, which has no window.claude use) inserts the link only."""
    head = tokens_link(rev) + ("\n" + SHIM.replace("__PAGE_REV__", rev) if shim_on else "")
    m = _HEAD_OPEN.match(html)   # the real <head> at the top of the document only: not a <header> element, not text that mentions the tag
    if m:
        return html[: m.end()] + "\n" + head + html[m.end():]
    return head + html


def rev_of(docs):
    h = hashlib.sha1()
    for d in docs:
        h.update(f"{d['id']}:{d['version']};".encode())
    return h.hexdigest()
