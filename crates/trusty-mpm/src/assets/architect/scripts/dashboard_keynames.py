"""dashboard_keynames.py: the Projects view's Secrets list, names only (GET /api/projects/key-names), read-only.

Why: #10350 (DOC-82 §10, owner ruling Dv3 2026-11-17): every user's install gets a Secrets list by owner/project on the
Architect dashboard's Projects view, showing key names and never a value. The full Secrets nav (dashboard_secrets.py,
with its write form) is not shipped (#10571), so this module is the read-only list that works on any install.
What: KeyNamesView.get() takes the tm-registered projects from the Projects view (dashboard_projects, so the list is
`tm project list` and nothing else), runs `tm secrets list` once per project whose root exists (cwd = that root, stdin
closed, TM_BIN overrides `tm`), and parses its "<vault> (Project|Owner):" headers and indented key lines with
parse_list(). Only a line whose first word is a valid trusty-secrets key name is kept, as {name, push_only}; every
other line is dropped, so a value can never pass through. tm's stdout text beyond those names and its stderr are never
returned: a failure is a fixed sentence with the exit status, and an exit 0 with no project vault header is the fixed
UNPARSEABLE error, never an empty list. The answer groups by GitHub owner:
  {"owners": [{key, owner, vaults: [{vault, names}], projects: [{id, label, vault, names, error}]}],
   "registry_error", "at"}
The owner vault is listed once per owner, not once per project. Projects with no GitHub URL sit under key "~none".
Answers are cached ANSWER_TTL_S; projects are listed MAX_WORKERS at a time.
Safety: values are never requested (`tm secrets list` prints names only) and never returned (the key-name filter).
Test: tests/test_dashboard_keynames.py
"""
import concurrent.futures
import os
import re
import subprocess
import threading
import time

ANSWER_TTL_S = 60.0
TIMEOUT_S = 30
MAX_WORKERS = 4
NONE_KEY = "~none"
# trusty-secrets SecretKey, optionally group-prefixed (group.KEY); fullmatch only
KEY_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,255}")
VAULT_LINE = re.compile(r"^(\S+) \((Project|Owner)\):$")
PUSH_ONLY = "(push-only for agents)"
UNPARSEABLE = ("Not listed: `tm secrets list` printed no project vault, so its output could not be read. "
               "Run it in the project folder to see why.")


def tm_bin():
    """The tm executable: $TM_BIN when set and non-empty, else `tm` on PATH."""
    return os.environ.get("TM_BIN") or "tm"


def run_tm(root):
    """(returncode, stdout) of `tm secrets list` run in `root`; (None, "") when tm cannot be started or times out.

    stderr is discarded: it is never shown, so it is never read."""
    try:
        p = subprocess.run([tm_bin(), "secrets", "list"], cwd=root, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                           stderr=subprocess.DEVNULL, text=True, timeout=TIMEOUT_S)
    except (OSError, subprocess.SubprocessError):
        return None, ""
    return p.returncode, p.stdout or ""


def parse_list(text):
    """[{vault, kind, names: [{name, push_only}]}] from `tm secrets list` output.

    A key line is kept only when its first word is a valid key name and the rest is empty or the push-only mark;
    any other line (a value, a stray message) is dropped. Lines before the first vault header are dropped."""
    out, cur = [], None
    for line in (text or "").splitlines():
        m = VAULT_LINE.match(line.strip())
        if m and not line.startswith(" "):
            cur = {"vault": m.group(1), "kind": m.group(2).lower(), "names": []}
            out.append(cur)
            continue
        if cur is None or not line.startswith("  "):
            continue
        body = line.strip()
        name, _, rest = body.partition(" ")
        rest = rest.strip()
        if KEY_NAME.fullmatch(name) and rest in ("", PUSH_ONLY):
            cur["names"].append({"name": name, "push_only": rest == PUSH_ONLY})
    return out


def _project_entry(p, runner):
    """One project's row: {id, label, vault, names, error} plus the owner-scope vaults it reported."""
    row = {"id": p["id"], "label": p.get("label") or p["id"], "vault": None, "names": [], "error": None}
    if not p.get("scanned"):
        row["error"] = "Not listed: this project's folder was not found on this machine."
        return row, []
    rc, out = runner(p["root"])
    if rc is None:
        row["error"] = "Not listed: tm could not be run (not on PATH, or it timed out)."
        return row, []
    if rc != 0:
        row["error"] = "Not listed: `tm secrets list` exited %d. Run it in the project folder for the reason." % rc
        return row, []
    owners = []
    for v in parse_list(out):
        if v["kind"] == "project" and row["vault"] is None:
            row["vault"], row["names"] = v["vault"], v["names"]
        elif v["kind"] == "owner":
            owners.append(v)
    if row["vault"] is None:   # #10350: tm's empty vault prints "  (no keys)" under a header; no header is unreadable, never empty
        row["error"] = UNPARSEABLE
        return row, []
    return row, owners


def build(projects, runner=run_tm, workers=MAX_WORKERS):
    """{"owners": [...]} for the Projects view's rows (dashboard_projects.build()["projects"]); see the module doc."""
    projects = [p for p in projects if isinstance(p, dict) and p.get("id")]
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, workers)) as ex:
        results = list(ex.map(lambda p: _project_entry(p, runner), projects))
    groups = {}
    for p, (row, owner_vaults) in zip(projects, results):
        key = p["owner"] if p.get("owner") and not p.get("no_github_url") else NONE_KEY
        g = groups.setdefault(key, {"key": key, "owner": None if key == NONE_KEY else key, "vaults": [], "projects": []})
        g["projects"].append(row)
        for v in owner_vaults:
            if all(x["vault"] != v["vault"] for x in g["vaults"]):
                g["vaults"].append({"vault": v["vault"], "names": v["names"]})
    for g in groups.values():
        g["projects"].sort(key=lambda r: (r["label"].lower(), r["id"].lower()))
    owners = sorted(groups.values(), key=lambda g: (g["key"] == NONE_KEY, g["key"].lower()))
    return {"owners": owners}


class KeyNamesView:
    """What the server holds: the Projects view as the project source, a runner, and an ANSWER_TTL_S answer cache."""

    def __init__(self, projects_view, runner=run_tm, clock=time.time):
        self.projects_view, self.runner, self.clock = projects_view, runner, clock
        self._at, self._val, self._lock = 0.0, None, threading.Lock()

    def get(self, refresh=False):
        """The cached answer; `refresh` rebuilds it. registry_error is carried over from the Projects view."""
        with self._lock:
            if refresh or self._val is None or self.clock() - self._at >= ANSWER_TTL_S:
                pv = self.projects_view.get({})
                val = build(pv.get("projects") or [], self.runner)
                val["registry_error"] = pv.get("registry_error")
                val["at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(self.clock()))
                self._val, self._at = val, self.clock()
            return self._val
