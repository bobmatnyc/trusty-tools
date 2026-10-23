#!/usr/bin/env python3
"""il-archive.py: archive Intent Ledger (IL) items to trusty-memory, then remove them (stdlib only).

Why: the owner 2026-10-17 03:56Z: "Decision archiving (any IL archiving) should be scripted." The Architect did it by hand
with dashboard_ids.py, trusty-memory and dashboard-db.py; this makes the order and the failure rules deterministic.
What: three verbs.
  dashboard retire --prefix D [--keep-free 64] [--dry-run]  done items chosen by dashboard_derive.retire_candidates
  dashboard prune [--days 7] [--dry-run]                    done/cancelled/wont_do items at least N days old whose
                                                            depends_on items are all done (or already gone)
  pm-ledger prune --project DIR [--days 7] [--dry-run]      archive.jsonl lines (<DIR>/.trusty-mpm/datg/, written by
                                                            datg close/prune) closed at least N days ago go to memory,
                                                            then leave archive.jsonl (SPEC-IL-01 section 3.4)
Each item becomes one memory line "<id> <created date> | <session> | <title> | answer: .. | outcome: ..", batched into
notes of at most ~9,000 characters and written with `trusty-memory note` (override the command with
IL_ARCHIVE_MEMORY_CMD). Memory is written FIRST; only then is the item deleted with its current version
(dashboard-db's delete also appends the full item to owner-queue-archive/pruned.jsonl). A failed memory write
deletes nothing for that note and the run exits 1; a version conflict keeps that item and exits 1.
Palace: the Architect's session (ARCHITECT_SESSION, default tm-architect) or blank -> the Architect's palace, the
config key memory_palace (architect_config: local/architect.json or ARCHITECT_MEMORY_PALACE; unset -> supervisor;
an unusable config exits 2 before any write). A project session maps through a line
"- trusty-memory palace: <name>" in local/projects/<project>.md (project = session minus tm- and -NN); no such line
-> the Architect's palace (printed). pm-ledger uses the same line for basename(DIR); none -> nothing is removed (printed),
because archive.jsonl is the only copy. Tags: archive, <dashboard|pm-ledger>, <retired-ids|pruned>, YYYY-MM-DD.
Root: --root or DASHBOARD_ROOT (default local/owner-queue). No secrets are read or printed.
Test: scripts/test_il_archive.py
"""
import argparse
import json
import os
import re
import shlex
import subprocess
import sys
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import architect_config  # noqa: E402
import dashboard_derive as derive  # noqa: E402
import dashboard_store as ds  # noqa: E402
import datg  # noqa: E402

NOTE_MAX = 9000
LINE_MAX = 2000
DEFAULT_PALACE = "supervisor"
PROJECTS_DIR = os.path.join(os.path.dirname(HERE), "local", "projects")
PALACE_RE = re.compile(r"^\s*[-*]?\s*trusty-memory palace:\s*([a-z0-9][a-z0-9_-]*)\s*$", re.I)
CLOSED = ("done", "cancelled", "wont_do")


class ArchiveError(Exception):
    """A memory write failed; nothing is deleted for that note."""


# ---- palace mapping ----------------------------------------------------------
def project_of(session):
    return re.sub(r"-\d+$", "", re.sub(r"^tm-", "", session or ""))


def palace_line(name, projects_dir):
    """Palace named by local/projects/<name>.md, or None."""
    try:
        with open(os.path.join(projects_dir, f"{name}.md"), encoding="utf-8") as f:
            for line in f:
                m = PALACE_RE.match(line)
                if m and m.group(1).lower() != "none":
                    return m.group(1)
    except OSError:
        pass
    return None


def architect_palace():
    """The Architect's palace: config key memory_palace, else supervisor. Raises architect_config.ConfigError."""
    # #10161: no palace name is built into a shipped script.
    return architect_config.load().memory_palace or DEFAULT_PALACE


def palace_for_session(session, projects_dir, home=DEFAULT_PALACE):
    """(palace, how). The Architect's session (ARCHITECT_SESSION, default tm-architect) / blank -> `home`;
    project session -> its file's palace line, else `home`."""
    # #10161: the session name comes from the environment, as in fleet-poll.py.
    if not session or session == (os.environ.get("ARCHITECT_SESSION") or "tm-architect"):
        return home, "architect/blank session"
    name = project_of(session)
    found = palace_line(name, projects_dir) if name else None
    if found:
        return found, f"local/projects/{name}.md"
    return home, f"default (no palace line for project {name})"


# ---- memory ------------------------------------------------------------------
def batch_lines(lines, limit=NOTE_MAX):
    """Group lines into bodies of at most `limit` characters (a line is cut to LINE_MAX)."""
    out, cur, size = [], [], 0
    for line in (x if len(x) <= LINE_MAX else x[:LINE_MAX - 3] + "..." for x in lines):
        if cur and size + len(line) + 1 > limit:
            out.append("\n".join(cur))
            cur, size = [], 0
        cur.append(line)
        size += len(line) + 1
    if cur:
        out.append("\n".join(cur))
    return out


def write_note(body, palace, tags):
    """Run `trusty-memory note`; return the first stdout line (the drawer id when the CLI prints one)."""
    cmd = shlex.split(os.environ.get("IL_ARCHIVE_MEMORY_CMD") or "trusty-memory")
    argv = [*cmd, "note", body, "--palace", palace]
    for t in tags:
        argv += ["--tag", t]
    try:
        p = subprocess.run(argv, capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.SubprocessError) as e:
        raise ArchiveError(f"memory write failed for palace {palace}: {e}")
    if p.returncode != 0:
        raise ArchiveError(f"memory write failed for palace {palace} (exit {p.returncode}): {p.stderr.strip()[:200]}")
    return (p.stdout.strip().splitlines() or [""])[0][:120]


def one_line(s):
    return " ".join(str(s or "").split())


def dash_line(doc):
    d = doc["data"]
    created = str(d.get("created") or "")[:10] or "?"
    return " | ".join([f"{doc['id']} {created}", one_line(d.get("session")) or "-", one_line(d.get("title")),
                       f"answer: {one_line(d.get('answer')) or '-'}", f"outcome: {one_line(d.get('outcome')) or '-'}"])


# ---- dashboard ---------------------------------------------------------------
def closed_at(d):
    wd = d.get("wont_do") if isinstance(d.get("wont_do"), dict) else {}
    return derive._ts(d.get("done_at") or d.get("cancelled_at") or wd.get("at") or d.get("updated") or d.get("created"))


def prune_candidates(snap, days, now):
    """Items closed at least `days` ago whose depends_on items are all done (a missing one counts as gone)."""
    status = {doc["id"]: derive._d(doc).get("status") for coll in snap for doc in snap[coll]}
    cutoff = now - timedelta(days=days)
    out = []
    for doc in snap.get("items", []):
        d = derive._d(doc)
        when = closed_at(d)
        if d.get("status") not in CLOSED or when is None or when > cutoff:
            continue
        deps = d.get("depends_on") or d.get("depends-on") or []
        if all(status.get(x, "done") == "done" for x in (deps if isinstance(deps, list) else [deps])):
            out.append(("items", doc))
    return sorted(out, key=lambda e: e[1]["id"])


def delete_items(store, entries):
    """Delete each (coll, doc) with its current version; a conflict or error keeps that item."""
    res = {"deleted": [], "failed": []}
    for coll, doc in entries:
        try:
            store.delete(coll, doc["id"], doc["version"])
            res["deleted"].append(doc["id"])
        except ds.StoreError as e:
            res["failed"].append((doc["id"], f"{e.code}: {e}"))
    return res


def archive_and_delete(store, entries, projects_dir, kind, dry_run, label, now, home=DEFAULT_PALACE):
    """Memory first, delete second, per palace and per note. `home` is the Architect's palace. Returns the exit code."""
    ids = [d["id"] for _, d in entries]
    print(f"{'dry-run: would archive' if dry_run else 'archiving'} {label}: {len(ids)} items: {', '.join(ids) or '-'}")
    if not entries:
        return 0
    groups = {}
    for coll, doc in entries:
        palace, how = palace_for_session(derive._d(doc).get("session"), projects_dir, home)
        groups.setdefault(palace, {"how": set(), "docs": []})
        groups[palace]["how"].add(how)
        groups[palace]["docs"].append((coll, doc))
    tags = ["archive", "dashboard", kind, now.strftime("%Y-%m-%d")]
    deleted, failed, rc = [], [], 0
    for palace, g in groups.items():
        group, how = g["docs"], "; ".join(sorted(g["how"]))
        bodies = batch_lines([dash_line(doc) for _, doc in group])
        print(f"palace {palace} (mapping: {how}): {len(group)} items in {len(bodies)} notes")
        if dry_run:
            continue
        pos = 0
        for body in bodies:
            n = len(body.splitlines())
            chunk, pos = group[pos:pos + n], pos + n
            try:
                drawer = write_note(body, palace, tags)
            except ArchiveError as e:
                print(f"error: {e}; nothing deleted for {', '.join(d['id'] for _, d in chunk)}", file=sys.stderr)
                return 1
            print(f"note written palace={palace} items={len(chunk)} cli={drawer or '-'}")
            res = delete_items(store, chunk)
            deleted += res["deleted"]
            failed += res["failed"]
    if not dry_run:
        print(f"deleted {len(deleted)}: {', '.join(deleted) or '-'}")
        for i, msg in failed:
            print(f"kept {i}: {msg}", file=sys.stderr)
            rc = 1
    return rc


def cmd_dashboard(a, now):
    try:
        home = architect_palace()
    except architect_config.ConfigError as e:   # the message names the path and key, never a value
        print(f"error: {e}", file=sys.stderr)
        return 2
    store = ds.Store(a.root)
    if a.action == "retire":
        snap = derive.snapshot(store, derive.ID_COLLECTIONS)
        want = set(derive.retire_candidates(snap, a.prefix, a.keep_free))
        entries = sorted(((c, doc) for c in derive.ID_COLLECTIONS for doc in snap[c] if doc["id"] in want
                          and derive._d(doc).get("status") == "done"), key=lambda e: e[1]["id"])
        return archive_and_delete(store, entries, a.projects_dir, "retired-ids", a.dry_run, f"retire {a.prefix}", now,
                                  home)
    snap = {"items": store.list_docs("items")}
    return archive_and_delete(store, prune_candidates(snap, a.days, now), a.projects_dir, "pruned", a.dry_run,
                              f"prune >={a.days}d", now, home)


# ---- per-PM ledger -----------------------------------------------------------
def ledger_line(kind, o, when):
    return " | ".join([f"{o['id']} {when.strftime('%Y-%m-%d')}", kind, one_line(o.get("title")),
                       f"answer: {one_line(o.get('answer')) or '-'}", f"outcome: {one_line(o.get('outcome')) or '-'}"])


def read_archive(path):
    """[(raw line, record or None)] of archive.jsonl; a line that is not JSON stays in the file untouched."""
    out = []
    try:
        with open(path, encoding="utf-8") as f:
            for ln in f.read().splitlines():
                try:
                    r = json.loads(ln)
                except ValueError:
                    r = None
                out.append((ln, r if isinstance(r, dict) else None))
    except OSError:
        pass
    return out


def cmd_pm_ledger(a, now):
    """SPEC-IL-01 section 3.4: archive.jsonl lines closed at least N days ago go to memory, then move to archive-<YYYYMM>.jsonl."""
    project = os.path.abspath(a.project)
    cutoff = now - timedelta(days=a.days)
    try:
        with datg.Locked(project):
            st = datg.load_state(project)
            d = datg.datg_dir(project)
            ap = os.path.join(d, "archive.jsonl")
            rows = read_archive(ap)
            picked = []   # (index in rows, kind, item, closed_at)
            for i, (_, r) in enumerate(rows):
                item = r.get("item") if r else None
                try:
                    when = datg.parse_iso(r["closed_at"]) if r else None
                except (KeyError, ValueError, TypeError):
                    when = None
                if isinstance(item, dict) and item.get("id") and when is not None and when <= cutoff:
                    picked.append((i, r.get("kind") or "item", item, when))
            ids = [it["id"] for _, _, it, _ in picked]
            print(f"{'dry-run: would archive' if a.dry_run else 'archiving'} pm-ledger {os.path.basename(project)}: "
                  f"{len(ids)} items: {', '.join(ids) or '-'}")
            if a.dry_run or not picked:
                return 0
            palace = palace_line(os.path.basename(project), a.projects_dir)
            if not palace:   # archive.jsonl is the only copy: without a memory write nothing may leave it
                print(f"no palace for project {os.path.basename(project)} in {a.projects_dir}; archive.jsonl kept")
                return 0
            tags = ["archive", "pm-ledger", "pruned", now.strftime("%Y-%m-%d")]
            for body in batch_lines([ledger_line(k, o, w) for _, k, o, w in picked]):
                print(f"note written palace={palace} cli={write_note(body, palace, tags) or '-'}")
            gone = {i for i, _, _, _ in picked}
            # the owner D2b 2026-10-17: no data loss path. Append the removed lines to archive-<YYYYMM>.jsonl, fsynced,
            # before archive.jsonl is rewritten; an OSError here leaves archive.jsonl untouched (exit 1).
            kept = os.path.join(d, f"archive-{now.strftime('%Y%m')}.jsonl")
            try:
                with open(kept, "a", encoding="utf-8") as kf:
                    kf.write("".join(ln + "\n" for i, (ln, _) in enumerate(rows) if i in gone))
                    kf.flush()
                    os.fsync(kf.fileno())
            except OSError as e:
                raise ArchiveError(f"cannot append to {kept}: {e}")
            datg._atomic_write(ap, "".join(ln + "\n" for i, (ln, _) in enumerate(rows) if i not in gone))
            datg.append_log(project, {"t": now.strftime("%Y-%m-%dT%H:%MZ"), "id": f"{st['code']}-archive", "ev": "update",
                                      "note": f"memory-archived {len(ids)}: {', '.join(ids)}"[:200]})
            print(f"removed {len(ids)} from {ap}; kept in {kept}")
    except datg.DatgError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    except ArchiveError as e:
        print(f"error: {e}; archive.jsonl untouched", file=sys.stderr)
        return 1
    return 0


def build_parser():
    ap = argparse.ArgumentParser(description="Archive Intent Ledger items to memory, then remove them.")
    ap.add_argument("--root", default=os.environ.get("DASHBOARD_ROOT", ds.DEFAULT_ROOT))
    ap.add_argument("--projects-dir", default=PROJECTS_DIR, help="where local/projects/<name>.md palace lines live")
    sub = ap.add_subparsers(dest="target", required=True)
    d = sub.add_parser("dashboard").add_subparsers(dest="action", required=True)
    r = d.add_parser("retire")
    r.add_argument("--prefix", required=True)
    r.add_argument("--keep-free", type=int, default=64)
    p = d.add_parser("prune")
    p.add_argument("--days", type=int, default=7)
    m = sub.add_parser("pm-ledger").add_subparsers(dest="action", required=True)
    mp = m.add_parser("prune")
    mp.add_argument("--project", required=True)
    mp.add_argument("--days", type=int, default=7)
    for x in (r, p, mp):
        x.add_argument("--dry-run", action="store_true")
    return ap


def main(argv=None):
    a = build_parser().parse_args(argv)
    now = datetime.now(timezone.utc)
    return cmd_dashboard(a, now) if a.target == "dashboard" else cmd_pm_ledger(a, now)


if __name__ == "__main__":
    sys.exit(main())
