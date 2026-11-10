#!/usr/bin/env python3
"""watch-apply.py: apply the owner's Watch / Unwatch requests (watch_changes docs) through the tm CLI.

Why: #10896, epic #10894 plan D6: `tm fleet project add|remove` is the only writer of local/watch.json. The dashboard
queues a request; this applier runs the CLI for it, so no pass edits the list by hand and the result is recorded.
What: reads the watch_changes docs with status "new" through dashboard_store. A doc whose action is not watch|unwatch,
or whose key starts with "-" or fails KEY_RE, is marked failed with a message and never reaches the CLI. The rest are
grouped by key; the newest doc (created, then id) decides the action, and one
`<TM_BIN or tm> fleet project add|remove --dir <dir> -- <key>` runs per key, with a timeout, from <dir>. Exit 0 marks
every doc of the group applied with the CLI's stdout; a non-zero exit, a timeout or a missing binary marks them failed
with the CLI's stderr, whitespace-collapsed and capped at MESSAGE_MAX characters. An unwatch of a key that an existing
local/watch.json does not hold is applied as "already not watched" with no CLI call: that covers a crash between the CLI
run and the status write. Each status write pins if_version; a doc changed meanwhile is left for the next run. Never
runs `tm fleet project list`, which creates watch.json when it is absent.
Exit: 0 every request applied (or none new); 1 at least one marked failed; 2 store error.
Root: --root or DASHBOARD_ROOT, default local/owner-queue. --dir defaults to this Architect directory.
Test: tests/test_watch_apply.py
"""
import argparse
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402

COLL = "watch_changes"
REPO = os.path.dirname(HERE)
WATCH = os.path.join("local", "watch.json")
# A registry key the CLI can name a record file after: one plain path component, no leading "-" or ".".
KEY_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")
VERBS = {"watch": "add", "unwatch": "remove"}
MESSAGE_MAX = 400
DEFAULT_TIMEOUT = 120


def cap(text):
    """One line, at most MESSAGE_MAX characters."""
    text = " ".join((text or "").split())
    return text if len(text) <= MESSAGE_MAX else text[: MESSAGE_MAX - 1] + "…"


def refusal(data):
    """Why a request must not reach the CLI, or None when it may."""
    action, key = data.get("action"), data.get("key")
    if action not in VERBS:
        return cap(f"unknown action {action!r}: use watch or unwatch")
    if not isinstance(key, str) or key.startswith("-") or not KEY_RE.match(key):
        return cap(f"refused key {key!r}: not a registry key")
    return None


def watched_keys(arch_dir):
    """Keys in <dir>/local/watch.json; None when the file is absent or unreadable (the CLI decides then)."""
    try:
        with open(os.path.join(arch_dir, WATCH), encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict) or data.get("version") != 1 or not isinstance(data.get("projects"), list):
        return None
    return {p.get("key") for p in data["projects"] if isinstance(p, dict)}


def run_cli(tm, verb, arch_dir, key, timeout):
    """(ok, message) for one `tm fleet project <verb>`; ok only on exit 0."""
    cmd = [tm, "fleet", "project", verb, "--dir", arch_dir, "--", key]
    try:
        proc = subprocess.run(cmd, cwd=arch_dir, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                              encoding="utf-8", errors="replace", timeout=timeout)
    except subprocess.TimeoutExpired:
        return False, f"tm fleet project {verb} {key}: timed out after {timeout}s"
    except FileNotFoundError:
        return False, f"{tm}: not found; set TM_BIN or put tm on PATH"
    except OSError as e:
        return False, cap(f"{tm}: {e}")
    if proc.returncode != 0:
        return False, cap(f"exit {proc.returncode}: {proc.stderr.strip() or proc.stdout.strip()}")
    return True, cap(proc.stdout) or "ok"


def plan(docs):
    """Split new docs into refusals [(doc, message)] and {key: [docs, oldest first]}."""
    refused, groups = [], {}
    for doc in docs:
        why = refusal(doc["data"])
        if why:
            refused.append((doc, why))
        else:
            groups.setdefault(doc["data"]["key"], []).append(doc)
    for group in groups.values():
        group.sort(key=lambda d: (str(d["data"].get("created", "")), d["id"]))
    return refused, groups


def mark(store, doc, status, message):
    """Write one outcome pinned to the version read; False when the doc changed meanwhile or the store refuses it."""
    try:
        store.update(COLL, doc["id"], {"status": status, "message": message, "done_at": ds.now_iso()},
                     if_version=doc["version"])
        return True
    except (ds.Conflict, ds.SchemaInvalid) as e:   # one bad doc never stops the others
        print(f"watch-apply: {doc['id']}: {e}; not written", file=sys.stderr)
        return False


def apply(store, arch_dir, tm, timeout):
    """Apply every new request; returns [(doc id, action, key, status, message)] for the docs written."""
    refused, groups = plan(store.list_docs(COLL, status="new"))
    results = []
    for doc, why in refused:
        if mark(store, doc, "failed", why):
            results.append((doc["id"], doc["data"].get("action"), doc["data"].get("key"), "failed", why))
    for key in sorted(groups):
        group = groups[key]
        action = group[-1]["data"]["action"]
        keys = watched_keys(arch_dir)
        if action == "unwatch" and keys is not None and key not in keys:
            ok, message = True, "already not watched"
        else:
            ok, message = run_cli(tm, VERBS[action], arch_dir, key, timeout)
        status = "applied" if ok else "failed"
        for doc in group:
            if mark(store, doc, status, message):
                results.append((doc["id"], action, key, status, message))
    return results


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", default=os.environ.get("DASHBOARD_ROOT") or ds.DEFAULT_ROOT)
    ap.add_argument("--dir", default=REPO, help="Architect directory (holds local/watch.json)")
    ap.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT, help="seconds per tm call")
    args = ap.parse_args(argv)
    tm = os.environ.get("TM_BIN") or "tm"
    try:
        results = apply(ds.Store(args.root, actor="watch-apply"), os.path.abspath(args.dir), tm, args.timeout)
    except (ds.StoreError, OSError) as e:
        print(f"watch-apply: store error: {e}", file=sys.stderr)
        return 2
    for doc_id, action, key, status, message in results:
        print(f"{doc_id} {action} {key}: {status}: {message}")
    return 1 if any(r[3] == "failed" for r in results) else 0


if __name__ == "__main__":
    sys.exit(main())
