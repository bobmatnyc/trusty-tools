#!/usr/bin/env python3
"""dashboard-db.py: CLI over the dashboard's file store (stdlib only); the Architect's replacement for ArtifactData.

Why: after the move to the localhost dashboard (the owner, 2026-10-08 17:34Z) the Architect
reads and writes dashboard documents with this command, with or without the server running.
What: get / list / set / update / delete / batch / next / create on <root>/<collection>/<id>.json through
dashboard_store (same flock, version check and atomic writes as dashboard-serve.py).
Output is JSON on stdout: get -> {id, version, data}; list -> [{id, version, data}];
writes -> {collection, id, version}; batch -> [results]. Errors go to stderr.
Exit codes: 0 ok, 1 not found, 2 invalid request or usage, 3 version conflict.
--json takes a file path, "-" for stdin, or a literal JSON string (starts with { or [).
batch FILE holds a list of ops, or {"ops": [...]}: {action: set|update|delete, collection,
id, data, if_version}
(the older key name the store writes is still read; see dashboard_store.BATCH_VERB_KEY). Root: --root or DASHBOARD_ROOT, default local/owner-queue.
Writes to collection "items" validate `steps`: each is an object with a non-empty string
`title` (fields: id, title, detail, links, values); a step with `text` and no `title` is refused.
set/create/update/batch on `items` print a stderr warning (never a refusal) when kind is question or action and `task` (a T id) is missing.
status-brief <T id> <MB id> <project/path.md> puts {id, at, brief} at the front of the task's `status_briefs` (newest first; schema-checked; the same id moves to the front).
Writes to collection "items" refuse an open/running task (kind task) left with no `steps` and no `progress` (ar-T33); `validate` lists any such open task.
Writes to collection "sessions" validate the `site` block (per-project live/staging/origin/default_branch/deploy_model/deploy_owner/notes + `src` map) against schemas/dashboard/sessions.schema.json; an invalid block is refused (exit 2).
Writes to collection "questions" (set, update on the merged doc, batch) validate against schemas/dashboard/questions.schema.json; an invalid doc is refused (exit 2).
tally [--json] counts open items by kind from the store (plus ", N answers waiting" for open `questions` docs, only when N > 0): the only source for dashboard counts quoted
in replies (parked items are counted apart). Prints "<N> open dashboard items: <q> questions, <a> actions, <t> tasks".
footer [--json] prints the one-line reply footer: "Dashboard: [local](...) · [tail](...) — <tally line>"; the tailnet part and the
tailnet half of `specs` normalisation need a tailnet origin: DASHBOARD_TAILNET_URL, else key tailnet_url of local/architect.json
(architect_config; e.g. https://<name>:7890/; neither = localhost only; an unusable config is named on stderr, #10161). Same tally() path; exit 2 with a stderr message when the store cannot be read (no guessed count). Replies quote it verbatim.
summary prints at most ~25 read-only lines for a resume: user_state (+ since), open D/A/T counts, the answers still to relay
(owner_answers.pending: id, kind, session, time, text cut to 60 chars) and the open `questions` count. Replaces `list items` in the resume prompt.
Qualified ids (ar-T44): get/update/delete accept "<S id>:<local id>" (e.g. S0c:D01) as well as the bare id; the S id must be the store's own session (Architect store = S14, a session store = its meta/state.session_id), else exit 1 with a hint naming it.
get/list on `items` add a derived `qualified` key (the store's S id, not the item's `session` field); stored ids stay bare. `set` on an existing doc without --if-version is refused (exit 3). `next PREFIX` prints the next free id;
`create COLLECTION PREFIX --json X` picks that id and writes it with if_version 0 in one command (use it for every new item).
log items <T id> "<text>" appends {at: now UTC, text} to that task's `log` (short accomplishment lines; the owner 2026-10-28) with a version-pinned update (retried once on a conflict); exit 2 on empty text or a non-items collection.
validate checks every document against schemas/dashboard/ (exit 1 on errors); fsck (jsonl store) checks canonical bytes and the change-log hashes.
Every delete (single or in a batch) first appends the full pre-delete document to
<root>/../owner-queue-archive/pruned.jsonl (pruned_at, actor, collection, id, version, data; flush + fsync);
if that append fails the delete is refused (exit 2). `pruned [--id ID] [--collection C]` prints matching lines, newest last.
--actor names the change-log actor (default architect). Format: DASHBOARD_FORMAT=dir|jsonl.
Test: tests/test_dashboard_store.py, tests/test_dashboard_tally.py, tests/test_dashboard_footer.py, tests/test_dashboard_jsonl.py, tests/test_dashboard_pruned.py, tests/test_dashboard_summary.py, tests/test_dashboard_qualified.py, tests/test_dashboard_specs_links.py, tests/test_dashboard_task_progress.py
Specs: writes to `items` (set, update, create, batch) normalize `specs`: one entry per local doc, localhost `#doc/` reader form (the page shows the tailnet twin); a localhost URL in `detail` becomes "see doc links" plus one spec.
"""
import argparse
import json
import re
from datetime import datetime, timezone
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import architect_config  # noqa: E402
import dashboard_derive as derive  # noqa: E402
import dashboard_jsonl as dj  # noqa: E402
import dashboard_schema as dsch  # noqa: E402
import dashboard_store as ds  # noqa: E402
import dashboard_qual as dq  # noqa: E402
import owner_answers as oa  # noqa: E402


def load_json(arg):
    text = arg.strip()
    if arg == "-":
        text = sys.stdin.read()
    elif not text.startswith(("{", "[")):
        try:
            with open(arg, "r", encoding="utf-8") as f:
                text = f.read()
        except OSError as e:
            raise ds.StoreError(f"cannot read {arg}: {e}")
    try:
        return json.loads(text)
    except ValueError as e:
        raise ds.StoreError(f"invalid JSON: {e}")


def dashboard_port():
    """local/architect.json `dashboard_port` (DASHBOARD_PORT wins), else 7890; an unusable config keeps 7890 (#10571)."""
    try:
        return architect_config.load().dashboard_port
    except architect_config.ConfigError:
        return architect_config.DEFAULT_PORT   # tailnet_host() names the failure on stderr


PORT = dashboard_port()
LOCAL_HOST = f"http://127.0.0.1:{PORT}/"


def tailnet_host():
    """The tailnet origin: DASHBOARD_TAILNET_URL, else architect.json `tailnet_url`, else "" (no tailnet link).

    An unusable config costs the tailnet link, never the command: the error (path and key, no value) goes to stderr
    and the environment variable alone decides (#10161).
    """
    try:
        return architect_config.load().tailnet_url
    except architect_config.ConfigError as e:
        sys.stderr.write(f"dashboard-db: tailnet link from DASHBOARD_TAILNET_URL only: {e}\n")
        return os.environ.get("DASHBOARD_TAILNET_URL", "")


TAIL_HOST = tailnet_host()
_LOCAL_URL = re.compile(r"http://127\.0\.0\.1:%d/[^\s)<>\"']+" % PORT)
_SUFFIX = re.compile(r"\s*\((?:localhost|tailnet)\)\s*$")


def reader_path(rest):
    """Path after the host -> reader form: `doc/<p>/<rel>` and `files/local/briefs/<f>` become `#doc/...`."""
    if rest.startswith("doc/"):
        return "#" + rest
    if rest.startswith("files/local/briefs/"):
        return "#doc/architect/" + rest[len("files/"):]
    return rest


def normalize_specs(data):
    """One spec entry per local doc, url in localhost reader form (the page adds the tailnet twin when it renders).
    Legacy /doc/ and /files/local/briefs/ forms are rewritten, "(localhost)"/"(tailnet)" label suffixes stripped, a
    localhost+tailnet pair for one path collapsed, a spec equal to the item's `brief` dropped. A localhost URL inside
    `detail` becomes "see doc links" and one spec. Other entries are untouched. Normalizes, never refuses."""
    if not isinstance(data, dict):
        return data
    specs, detail = data.get("specs"), data.get("detail")
    found = _LOCAL_URL.findall(detail) if isinstance(detail, str) else []
    if not isinstance(specs, list) and not found:
        return data
    brief = data.get("brief")
    out, seen = [], set()
    entries = list(specs) if isinstance(specs, list) else []
    entries += [{"label": u.rsplit("/", 1)[-1] or "doc", "url": u} for u in found]
    for e in entries:
        url = e.get("url") if isinstance(e, dict) else None
        host = next((h for h in (LOCAL_HOST, TAIL_HOST) if h and isinstance(url, str) and url.startswith(h)), None)
        if host is None:
            out.append(e)
            continue
        path = reader_path(url[len(host):])
        if path in seen or (isinstance(brief, str) and path == "#doc/" + brief):
            continue
        seen.add(path)
        out.append({**e, "label": _SUFFIX.sub("", str(e.get("label", ""))) or path.rsplit("/", 1)[-1], "url": LOCAL_HOST + path})
    data = dict(data)
    data["specs"] = out
    if found:
        data["detail"] = _LOCAL_URL.sub("see doc links", detail)
    return data


STEP_FIELDS = "id, title, detail, links, values"


def check_steps(item_id, data):
    """Refuse a malformed `steps` list (the page renders step.title, so a missing one shows blank)."""
    if not isinstance(data, dict) or "steps" not in data:
        return
    steps = data["steps"]
    if not isinstance(steps, list):
        raise ds.StoreError(f"item {item_id}: steps must be a list of objects with fields: {STEP_FIELDS}")
    for i, st in enumerate(steps):
        where = f"item {item_id}: step {i}"
        if not isinstance(st, dict):
            raise ds.StoreError(f"{where} must be an object with fields: {STEP_FIELDS}")
        title = st.get("title")
        if not isinstance(title, str) or not title.strip():
            hint = " ('text' is not rendered; rename it to 'title')" if "text" in st else ""
            raise ds.StoreError(f"{where} needs a non-empty string 'title'{hint}; expected fields: {STEP_FIELDS}")


def has_progress(data):
    """True when a task doc carries a progress figure: a non-empty `steps` list, or `progress` as a number or a dict with a numeric `pct`."""
    if isinstance(data.get("steps"), list) and data["steps"]:
        return True
    p = data.get("progress")
    if isinstance(p, dict):
        p = p.get("pct")
    return isinstance(p, (int, float)) and not isinstance(p, bool)


def untracked_open_tasks(docs):
    """Ids of open/running kind-task items with no steps and no progress (ar-T33: the owner 2026-10-30 "all tasks should be progress tracked")."""
    return sorted(d["id"] for d in docs if isinstance(d.get("data"), dict) and d["data"].get("kind") == "task"
                  and d["data"].get("status") in ("open", "running") and not has_progress(d["data"]))


def check_task_progress(store, verb, doc_id, data):
    """Refuse a write that leaves an open/running task with no steps and no progress (ar-T33); D, A, G, Q and closed tasks are not checked. update checks the merged doc."""
    if not isinstance(data, dict):
        return
    if verb == "update":
        try:
            data = {**store.get("items", doc_id)["data"], **data}
        except ds.StoreError:
            pass
    if data.get("kind") == "task" and data.get("status") in ("open", "running") and not has_progress(data):
        raise ds.StoreError(f"item {doc_id}: every open task needs progress; add `steps` (one per remaining action, objects with fields: {STEP_FIELDS}) or `progress` {{pct: 0-100}}")


def check_question(store, verb, doc_id, data):
    """Refuse a write that would leave a `questions` doc invalid against its schema (update validates the merged doc)."""
    if verb == "update":
        try:
            data = {**store.get("questions", doc_id)["data"], **(data if isinstance(data, dict) else {})}
        except ds.StoreError:
            pass   # missing doc: the store reports it
    data = {k: v for k, v in data.items() if k != "version"} if isinstance(data, dict) else data
    errs = dsch.validate("questions", doc_id, data)
    if errs:
        raise ds.StoreError("questions write refused by schema: " + "; ".join(errs))


def check_site(store, verb, doc_id, data):
    """Refuse a `sessions` write that leaves its `site` block (live/staging/origin/deploy facts) invalid against the schema; other fields are not this write's concern."""
    if not isinstance(data, dict) or "site" not in data:
        return
    errs = [e for e in dsch.validate("sessions", doc_id, {**data, "name": "x", "state": "idle", "status": "", "summary": "", "updated": "2000-01-01T00:00:00Z"}) if ": /site" in e]
    if errs:
        raise ds.StoreError("sessions write refused by schema: " + "; ".join(errs))


def warn_no_task(store, verb, doc_id, data):
    """Warn on stderr (never refuse) when a question/action item has no `task` (the owner 2026-10-28: every D names its T, every Blocker the T it unblocks). update checks the merged doc."""
    if not isinstance(data, dict):
        return
    if verb == "update":
        try:
            data = {**store.get("items", doc_id)["data"], **data}
        except ds.StoreError:
            pass
    if data.get("kind") in ("question", "action") and not str(data.get("task") or "").strip():
        print(f"warning: item {doc_id} (kind {data['kind']}) has no `task`; a decision names its T and a blocker names the T it unblocks", file=sys.stderr)


RETIRED_PREFIX = "A"   # Blockers use B since the owner 2026-10-30 (Dd1); A stays reserved, never reissued


def check_prefix_open(prefix):
    """Refuse minting a new id with the retired Blocker prefix."""
    if prefix == RETIRED_PREFIX:
        raise ds.StoreError("prefix A is retired; use B (blockers)")


def check_set_new(store, coll, doc_id, if_version):
    """Refuse a `set` that would replace an existing doc without --if-version (a hand-typed id landed on a live entry, 2026-10-20 Dd3)."""
    if coll == "items" and re.fullmatch(RETIRED_PREFIX + r"[0-9a-f]{2}", doc_id or "") and dq.alias_rules(store):
        raise ds.StoreError(f"set {coll}/{doc_id}: prefix A is retired; use B")
    if dq.split(doc_id)[0]:
        raise ds.StoreError(f"set {coll}/{doc_id}: a qualified id is a reference, not a new id; use `create` or a bare id")
    if if_version is not None:
        return
    try:
        store.get(coll, doc_id)
    except ds.NotFound:
        return
    e = ds.Conflict(f"{coll}/{doc_id} already exists; pass --if-version <v> to replace it, or `create` for a new id")
    raise e


def verb_of(entry):
    """A batch entry's verb: `action`, else the store's own key (#10571; older files keep working)."""
    return entry.get("action", entry.get(ds.BATCH_VERB_KEY))


def to_store_entries(entries):
    """Each dict entry with its verb under the store's key (`action` moved there); other values pass through."""
    out = []
    for entry in entries if isinstance(entries, list) else []:
        if isinstance(entry, dict) and "action" in entry and ds.BATCH_VERB_KEY in entry:
            raise ds.StoreError("a batch entry names its verb twice (both `action` and the older key); keep one")
        if isinstance(entry, dict) and "action" in entry:
            entry = {(ds.BATCH_VERB_KEY if k == "action" else k): v for k, v in entry.items()}
        out.append(entry)
    return out if isinstance(entries, list) else entries


def check_ops(ops, store=None):
    for entry in ops if isinstance(ops, list) else []:
        if not isinstance(entry, dict) or verb_of(entry) not in ("set", "update"):
            continue
        verb = verb_of(entry)
        if entry.get("collection") == "items":
            check_steps(entry.get("id"), entry.get("data"))
            if store is not None:
                check_task_progress(store, verb, entry.get("id"), entry.get("data"))
                warn_no_task(store, verb, entry.get("id"), entry.get("data"))
        elif entry.get("collection") == "questions" and store is not None:
            check_question(store, verb, entry.get("id"), entry.get("data"))
        elif entry.get("collection") == "sessions" and store is not None:
            check_site(store, verb, entry.get("id"), entry.get("data"))


def tally(store):
    """Open counts by kind (parked items counted apart), plus answered-not-relayed: dashboard_derive.tally."""
    docs = store.list_docs("items")
    out = derive.tally(docs)
    out["running"] = derive.running_count(docs, datetime.now(timezone.utc))
    waiting = sum(1 for d in store.list_docs("questions") if (d.get("data") or {}).get("status") == "open")
    if waiting:   # key absent when 0, so the output of a store with no open answers is unchanged
        out["answers_waiting"] = waiting
    return out


def tally_line(t):
    def n(v, word):
        return f"{v} {word}" + ("" if v == 1 else "s")
    return (f"{t['open']} open dashboard item{'' if t['open'] == 1 else 's'}: "
            f"{n(t['questions'], 'question')}, {n(t['actions'], 'action')}, {n(t['tasks'], 'task')}"
            + (f" ({t['parked']} parked)" if t.get("parked") else "")
            + (f" ({t['running']} running)" if t.get("running") else "")
            + (f", {t['answers_waiting']} answers waiting" if t.get("answers_waiting") else ""))


SUMMARY_MAX_ANSWERS = 18


def summary_lines(store):
    """Read-only resume summary lines (<= ~25). Raises StoreError when unreadable."""
    try:
        settings = store.get("meta", "settings")["data"]
    except ds.NotFound:
        settings = {}
    out = [f"user_state: {settings.get('user_state', 'active')} since {settings.get('user_state_at', 'unknown')}"]
    docs = store.list_docs("items")
    t = derive.tally(docs)
    out.append(f"open: {t['open']} (D {t['questions']}, A {t['actions']}, T {t['tasks']})")
    by_id = {d["id"]: (d.get("data") or {}) for d in docs}
    pend = oa.pending(store.root)
    sid = dq.store_sid(store)
    out.append(f"answers to relay: {len(pend)}")
    for e in pend[:SUMMARY_MAX_ANSWERS]:
        text = " ".join(str(e.get("text", "")).split())
        text = text if len(text) <= 60 else text[:59] + "\u2026"
        sess = by_id.get(e['id'], {}).get('session')
        out.append(f"  {dq.qualify(sid, e['id'])} {e['kind']} {sess or '-'} {e.get('at', '')} {text}")
    if len(pend) > SUMMARY_MAX_ANSWERS:
        out.append(f"  ... {len(pend) - SUMMARY_MAX_ANSWERS} more (python3 scripts/pending-answers.py)")
    qopen = sum(1 for d in store.list_docs("questions")
                if (d.get("data") or {}).get("status") not in ("cleared", "pending") and not (d.get("data") or {}).get("cleared_at"))
    out.append(f"questions open: {qopen}")
    return out


LOCAL_URL = LOCAL_HOST
TAILNET_URL = TAIL_HOST


def footer(store):
    """Reply footer from the one tally path: ({"footer": line, "tally": t}). Raises StoreError when unreadable."""
    if not os.path.isdir(store.root):
        raise ds.StoreError(f"store root not readable: {store.root}")
    try:
        t = tally(store)
    except (OSError, ValueError) as e:   # JSONDecodeError is a ValueError
        raise ds.StoreError(f"store not readable: {e}")
    tail = f" \u00b7 [tail]({TAILNET_URL})" if TAILNET_URL else ""
    line = f"Dashboard: [local]({LOCAL_URL}){tail} \u2014 {tally_line(t)}"
    return {"footer": line, "tally": t}


def pruned_lines(store, doc_id=None, coll=None):
    """Raw prune-log lines (oldest first, so newest last) matching --id / --collection."""
    try:
        with open(store.prune_log_path(), encoding="utf-8") as f:
            raw = f.read().splitlines()
    except FileNotFoundError:
        return []
    out = []
    for line in raw:
        rec = json.loads(line)
        if (doc_id is None or rec.get("id") == doc_id) and (coll is None or rec.get("collection") == coll):
            out.append(line)
    return out


def emit(obj):
    json.dump(obj, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")


def append_log(store, collection, id_, text, now=None):
    """Append {at, text} to an item's `log`, pinned to the version read; one retry when another writer got in first."""
    if collection != "items":
        raise ds.StoreError("log applies to the items collection")
    text = (text or "").strip()
    if not text:
        raise ds.StoreError("log text is empty")
    entry = {"at": (now or datetime.now(timezone.utc)).strftime("%Y-%m-%dT%H:%M:%SZ"), "text": text}
    id_ = dq.resolve(store, collection, id_)
    for attempt in (0, 1):
        doc = store.get(collection, id_)
        data = doc["data"] if isinstance(doc.get("data"), dict) else doc
        cur = data.get("log") if isinstance(data.get("log"), list) else []
        try:
            return store.update(collection, id_, {"log": cur + [entry]}, doc["version"])
        except ds.Conflict:
            if attempt:
                raise


def append_status_brief(store, id_, brief_id, brief, now=None):
    """Put {id, at, brief} at the FRONT of a task's `status_briefs` (newest first); a repeated id moves to the front. The new list is checked against the items schema; pinned to the version read, one retry."""
    entry = {"id": (brief_id or "").strip(), "at": (now or datetime.now(timezone.utc)).strftime("%Y-%m-%dT%H:%M:%SZ"), "brief": (brief or "").strip()}
    id_ = dq.resolve(store, "items", id_)
    for attempt in (0, 1):
        doc = store.get("items", id_)
        data = doc["data"] if isinstance(doc.get("data"), dict) else doc
        cur = data.get("status_briefs") if isinstance(data.get("status_briefs"), list) else []
        merged = dict(data, status_briefs=[entry] + [e for e in cur if not (isinstance(e, dict) and e.get("id") == entry["id"])])
        errs = [e for e in dsch.validate("items", id_, merged) if ": /status_briefs" in e]   # only this field; other gaps in a legacy doc are not this write's
        if errs:
            raise ds.StoreError("; ".join(errs))
        try:
            return store.update("items", id_, {"status_briefs": merged["status_briefs"]}, doc["version"])
        except ds.Conflict:
            if attempt:
                raise


def build_parser():
    ap = argparse.ArgumentParser(description="Read and write Architect dashboard documents.")
    ap.add_argument("--root", default=os.environ.get("DASHBOARD_ROOT", ds.DEFAULT_ROOT))
    ap.add_argument("--actor", default=os.environ.get("DASHBOARD_ACTOR", "architect"), help="change-log actor (jsonl store)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("get"); g.add_argument("collection"); g.add_argument("id")
    ls = sub.add_parser("list"); ls.add_argument("collection"); ls.add_argument("--status")
    for name in ("set", "update"):
        w = sub.add_parser(name); w.add_argument("collection"); w.add_argument("id")
        w.add_argument("--json", required=True, metavar="FILE_OR_STRING")
        w.add_argument("--if-version", type=int)
    d = sub.add_parser("delete"); d.add_argument("collection"); d.add_argument("id"); d.add_argument("--if-version", type=int)
    pr = sub.add_parser("pruned"); pr.add_argument("--id"); pr.add_argument("--collection")
    n = sub.add_parser("next"); n.add_argument("prefix", help="id prefix letter, e.g. D")
    c = sub.add_parser("create"); c.add_argument("collection"); c.add_argument("prefix"); c.add_argument("--json", required=True, metavar="FILE_OR_STRING")
    lg = sub.add_parser("log"); lg.add_argument("collection"); lg.add_argument("id"); lg.add_argument("text")
    sb = sub.add_parser("status-brief", help="prepend a status brief to a task: status-brief T1c MB0be9 cto/docs/briefs/x.md"); sb.add_argument("id"); sb.add_argument("brief_id"); sb.add_argument("brief")
    sub.add_parser("summary")
    sub.add_parser("validate")
    sub.add_parser("fsck")
    t = sub.add_parser("tally"); t.add_argument("--json", action="store_true", dest="as_json")
    f = sub.add_parser("footer"); f.add_argument("--json", action="store_true", dest="as_json")
    b = sub.add_parser("batch"); b.add_argument("file", help="JSON file (or - for stdin): list of ops or {ops: [...]}")
    return ap


def run(args):
    store = ds.Store(args.root, actor=args.actor)
    if args.cmd == "fsck":
        if store.fmt != "jsonl":
            raise ds.StoreError("fsck needs DASHBOARD_FORMAT=jsonl")
        return {"problems": dj.fsck(store.root)}
    if args.cmd == "validate":
        snap = {c: store.list_docs(c) for c in store.collections()}
        errs = dsch.validate_store(snap) + [f"items/{i}: open task has no steps and no progress" for i in untracked_open_tasks(snap.get("items", []))]
        return {"errors": errs, "documents": sum(len(v) for v in snap.values())}
    if args.cmd == "pruned":
        return pruned_lines(store, args.id, args.collection)
    if args.cmd in ("next", "create"):
        check_prefix_open(args.prefix)
    if args.cmd == "next":
        n = derive.next_id(derive.snapshot(store, derive.ID_COLLECTIONS), args.prefix)
        if n is None:
            raise ds.StoreError(f"no free {args.prefix} ids", )
        return n
    if args.cmd == "create":
        n = derive.next_id(derive.snapshot(store, derive.ID_COLLECTIONS), args.prefix)
        if n is None:
            raise ds.StoreError(f"no free {args.prefix} ids")
        data = load_json(args.json)
        if args.collection == "items":
            check_steps(n, data)
            check_task_progress(store, "create", n, data)
            warn_no_task(store, "create", n, data)
            data = normalize_specs(data)
        return store.set(args.collection, n, data, 0)   # if_version 0: a concurrent create of the same id conflicts
    if args.cmd == "get":
        local = dq.resolve(store, args.collection, args.id)
        doc = dq.with_qualifier(store, args.collection, store.get(args.collection, local))
        asked = dq.split(args.id)[1]
        return dict(doc, alias_of=asked) if asked != local else doc
    if args.cmd == "list":
        return dq.with_qualifier(store, args.collection, store.list_docs(args.collection, status=args.status))
    if args.cmd in ("set", "update"):
        if args.cmd == "update":
            args.id = dq.resolve(store, args.collection, args.id)
        else:
            check_set_new(store, args.collection, args.id, args.if_version)
        fn = store.set if args.cmd == "set" else store.update
        data = load_json(args.json)
        if args.collection == "items":
            check_steps(args.id, data)
            check_task_progress(store, args.cmd, args.id, data)
            warn_no_task(store, args.cmd, args.id, data)
            data = normalize_specs(data)
        elif args.collection == "questions":
            check_question(store, args.cmd, args.id, data)
        elif args.collection == "sessions":
            check_site(store, args.cmd, args.id, data)
        return fn(args.collection, args.id, data, args.if_version)
    if args.cmd == "log":
        return append_log(store, args.collection, args.id, args.text)
    if args.cmd == "status-brief":
        return append_status_brief(store, args.id, args.brief_id, args.brief)
    if args.cmd == "delete":
        return store.delete(args.collection, dq.resolve(store, args.collection, args.id), args.if_version)
    if args.cmd == "tally":
        return tally(store)
    if args.cmd == "footer":
        return footer(store)
    if args.cmd == "summary":
        return summary_lines(store)
    ops = load_json(args.file)
    ops = to_store_entries(ops["ops"] if isinstance(ops, dict) and "ops" in ops else ops)
    check_ops(ops, store)
    for entry in ops if isinstance(ops, list) else []:
        if isinstance(entry, dict) and verb_of(entry) in ("set", "update") and entry.get("collection") == "items":
            entry["data"] = normalize_specs(entry.get("data"))
    for entry in ops if isinstance(ops, list) else []:
        if isinstance(entry, dict) and verb_of(entry) == "set" and "collection" in entry and "id" in entry:
            check_set_new(store, entry["collection"], entry["id"], entry.get("if_version"))
    return store.batch(ops)


def main(argv=None):
    args = build_parser().parse_args(argv)
    try:
        out = run(args)
        if args.cmd in ("fsck", "validate"):
            emit(out)
            return 1 if out.get("problems") or out.get("errors") else 0
        if args.cmd == "pruned":
            sys.stdout.write("".join(x + "\n" for x in out))
        elif args.cmd == "tally" and not args.as_json:
            sys.stdout.write(tally_line(out) + "\n")
        elif args.cmd == "summary":
            sys.stdout.write("".join(x + "\n" for x in out))
        elif args.cmd == "footer" and not args.as_json:
            sys.stdout.write(out["footer"] + "\n")
        else:
            emit(out)
    except ds.StoreError as e:
        sys.stderr.write(f"dashboard-db: {e.code}: {e}\n")
        return {404: 1, 409: 3}.get(e.status, 2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
