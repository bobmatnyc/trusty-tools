#!/usr/bin/env python3
"""Write the dashboard store goldens from the prototype writer (#10063 PR 1).

Why: the Rust store must leave the prototype's bytes in both formats
(DOC-82 LDG-3.2, #10063 AC 2), and its conflict, batch and refusal outcomes
must match (LDG-2.8.1, AC 3). Only the prototype can say what those bytes are.
What: builds three cases of op batches, runs each through the prototype's
`dashboard_store.Store` twice (format `dir`, format `jsonl`, schema check off,
prune log outside the root), and writes beside this file:
`corpus.json` (each case's batches with the prototype's per-format outcome:
the results list, or the error class and message) and `<case>/dir/...`,
`<case>/jsonl/...` (every store file after the last batch, minus `.lock` and
the change log, which PR 2 owns), plus each case's `derived` values over the
final store: next/free/retire ids per prefix, the tally, its line and the
footer at a pinned clock (NOW) with neutral links (LINKS), and every
document's schema errors against `<proto>/schemas/dashboard/`.
Cases: `synthetic` (the prototype's own fixture store) and `live` (a sample
of a live store), both scrubbed with ../ledger/scrub.py's Scrubber (free
text becomes filler, bare ids are renumbered dense per prefix, other doc ids
become `doc-NNN`, session names `pm-N`; a value under an enum key is kept only
when it is an enum token), and `edge` (values chosen to break a naive
encoder, plus every refusal class).
Deterministic: the same inputs give the same bytes.
Test: `dashboard_writer_matches_prototype_bytes`, `dashboard_fixtures_are_scrubbed`,
`derived_values_match_prototype`, `prototype_schemas_load_and_fixtures_validate`.

Run from the repository root:
    python3 -B -I crates/trusty-architect/tests/fixtures/dashboard/regen_dashboard.py \
        --proto <architect repo> --live <store dir> [--deny-out <path outside the repo>]
"""
import argparse
import json
import os
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SAMPLE = {"items": 40, "sessions": 10, "questions": 12}
KEEP = {"status", "kind", "lifecycle", "state", "live_state", "choice", "user_state", "ev", "mode"}
META_IDS = ("state", "settings", "seen", "gh_track")


NOW = "2026-10-21T10:00:00Z"
LINKS = ("http://127.0.0.1:7890/", "https://dash.example.ts.net:7890/")
PREFIXES = "GPADTSQR"


def load_proto(proto):
    sys.path.insert(0, os.path.join(proto, "scripts"))
    import dashboard_store  # noqa: E402

    return dashboard_store


def load_db(proto):
    """dashboard-db.py with a pinned clock and neutral footer links."""
    import datetime as real
    import importlib.util

    spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(proto, "scripts", "dashboard-db.py"))
    db = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(db)

    class Pinned(real.datetime):
        @classmethod
        def now(cls, tz=None):
            return real.datetime(2026, 10, 21, 10, 0, 0, tzinfo=tz)

    db.datetime = Pinned
    db.LOCAL_URL, db.TAILNET_URL = LINKS
    return db


def derived(proto, ds, root):
    """The prototype's ids, tally, footer and schema errors over the dir store at root."""
    import dashboard_derive as derive
    import dashboard_schema as dsch

    db = load_db(proto)
    store = ds.Store(root, fmt="dir", schema_mode="off")
    snap = derive.snapshot(store, derive.ID_COLLECTIONS)
    footer = db.footer(store)
    docs = {c: store.list_docs(c) for c in store.collections()}
    return {
        "next": {p: derive.next_id(snap, p) for p in PREFIXES},
        "free": {p: derive.free_count(snap, p) for p in PREFIXES},
        "retire": {p: derive.retire_candidates(snap, p) for p in PREFIXES},
        "tally": footer["tally"],
        "tally_line": db.tally_line(footer["tally"]),
        "footer": footer["footer"],
        "schema_errors": {f"{c}/{d['id']}": dsch.validate(c, d["id"], d["data"]) for c in docs for d in docs[c]},
    }


def load_scrubber():
    """scrub.py's Scrubber, keeping a value under a KEEP key only when it is an
    enum token (`sessions.status`, for one, is free text)."""
    import re

    sys.path.insert(0, os.path.join(HERE, "..", "ledger"))
    import scrub  # noqa: E402

    scrub.KEEP = set(scrub.KEEP) | KEEP
    enum = re.compile(r"^[a-z_]{1,24}$")

    class DashScrubber(scrub.Scrubber):
        def value(self, key, v):
            if isinstance(v, str) and key in scrub.KEEP and v and not enum.match(v) and not scrub.TS.match(v):
                return self.filler(v)
            return super().value(key, v)

    return DashScrubber()


def read_dir_store(root):
    """{coll: {id: data}} of a dir-format store, `version` dropped, sorted."""
    out = {}
    for coll in sorted(os.listdir(root)):
        cdir = os.path.join(root, coll)
        if coll.startswith(".") or not os.path.isdir(cdir):
            continue
        docs = {}
        for name in sorted(os.listdir(cdir)):
            if name.endswith(".json") and not name.startswith("."):
                with open(os.path.join(cdir, name), encoding="utf-8") as f:
                    data = json.load(f)
                data.pop("version", None)
                docs[name[:-5]] = data
        if docs:
            out[coll] = docs
    return out


def set_ops(store):
    return [{"op": "set", "collection": c, "id": i, "data": store[c][i]} for c in sorted(store) for i in sorted(store[c])]


def scrubbed(real, scrubber):
    """{coll: {id: data}} with every doc id and value scrubbed (see the module doc)."""
    out = {}
    for coll in sorted(real):
        docs = {}
        for k, doc_id in enumerate(sorted(real[coll])):
            if coll == "meta" and doc_id in META_IDS:
                new_id = doc_id
            elif scrub_idish(doc_id):
                new_id = scrubber.value("id", doc_id)
            else:
                scrubber.deny.add(doc_id)
                new_id = f"doc-{k + 1:03d}"
            docs[new_id] = scrubber.value("", real[coll][doc_id])
        out[coll] = docs
    return out


def synthetic_case(proto, scrubber):
    # The prototype's synthetic store names a real account and time zone; scrub it too.
    real = read_dir_store(os.path.join(proto, "scripts", "fixtures", "dashboard", "store"))
    store = scrubbed(real, scrubber)
    items = sorted(store["items"])
    first, last = items[0], items[-1]
    return [
        set_ops(store),
        [
            {"op": "update", "collection": "items", "id": first, "if_version": 1,
             "data": {"answer": "Hold", "updated": "2026-10-12T03:46Z"}},
            {"op": "delete", "collection": "items", "id": last},
        ],
        [{"op": "update", "collection": "items", "id": first, "if_version": 1, "data": {"answer": "stale"}}],
    ]


def live_case(live, scrubber):
    real = read_dir_store(live)
    for coll in real:
        ids = sorted(real[coll])
        n = SAMPLE.get(coll)
        if n and len(ids) > n:
            step = len(ids) / n
            real[coll] = {i: real[coll][i] for i in (ids[int(k * step)] for k in range(n))}
    return [set_ops(scrubbed(real, scrubber))]


def scrub_idish(doc_id):
    return len(doc_id) == 3 and doc_id[0].isupper() and all(c in "0123456789abcdef" for c in doc_id[1:])


EDGE_DOC = {
    "floats": [0.0, -0.0, 0.1, 1.5, 1e16, 1e15, 123456789012345.6, 1.5e-05, 0.0001, 1e300, 2.5e-300,
               5e-324, 1.7976931348623157e308, -2.75, 100.0, 1e22],
    "ints": [0, -1, 9007199254740993, 9223372036854775807, -9223372036854775808, 18446744073709551615],
    "strings": ["\x00\x01\x1f\x7f", "tab\there\r\n", "  ", "é ü", "😀 astral", "quote \" slash \\ /",
                "<script>&amp;</script>", ""],
    "keys": {"b": 1, "a": 2, "é": 3, "Z": 4, "😀": 5, "aa": 6, "": 7, "A": 8, "_": 9},
    "nested": {"empty_obj": {}, "empty_list": [], "t": True, "f": False, "n": None,
               "deep": [[{"z": [1, {"y": None}]}]]},
    "created": "2026-10-12T03:46Z",
    "updates": [{"at": "2026-10-12T03:47Z", "note": "2026-10-12T03:48Z"}],
    "title": "2026-10-12T03:49Z",
    "done_at": "2026-10-12T03:50Z",
    "due": "2026-10-12T03:51:00Z",
}


def edge_case():
    ids = ["A01", "a02", "Zed", "zee", "0x", "a.b", "a-b", "a_b", "B"]
    return [
        [{"op": "set", "collection": "edge", "id": i, "data": dict(EDGE_DOC, n=k)} for k, i in enumerate(ids)],
        [{"op": "set", "collection": "edge", "id": "A01", "if_version": 0, "data": {"x": 1}}],
        [{"op": "set", "collection": "edge", "id": "C01", "data": {"x": 1}},
         {"op": "update", "collection": "edge", "id": "missing", "data": {"x": 2}}],
        [{"op": "set", "collection": "edge", "id": "C02", "data": {"x": 1}},
         {"op": "set", "collection": "edge", "id": "C03", "data": {"x": 1, "version": 4}}],
        [{"op": "set", "collection": "edge", "id": "C04", "data": [1, 2]}],
        [{"op": "set", "collection": "edge", "id": "../x", "data": {}}],
        [{"op": "set", "collection": "edge", "id": ".hidden", "data": {}}],
        [{"op": "set", "collection": "edge", "id": "A01", "if_version": True, "data": {}}],
        [{"op": "rename", "collection": "edge", "id": "A01"}],
        [{"op": "update", "collection": "edge", "id": "A01", "if_version": 1,
          "data": {"floats": None, "added": "x", "until": "2026-10-12T04:00Z"}},
         {"op": "update", "collection": "edge", "id": "A01", "data": {"chained": True}},
         {"op": "delete", "collection": "edge", "id": "a02", "if_version": 1}],
        [{"op": "delete", "collection": "edge", "id": "a02"}],
        [{"op": "set", "collection": "solo", "id": "S01", "data": {"x": 1}}],
        [{"op": "delete", "collection": "solo", "id": "S01"}],
        [{"op": "set", "collection": "other", "id": "O01", "data": {"x": 1}},
         {"op": "delete", "collection": "edge", "id": "Zed", "if_version": 9}],
    ]


def outcome(store, ops):
    try:
        return {"ok": store.batch(json.loads(json.dumps(ops)))}
    except Exception as e:  # noqa: BLE001 - every class is recorded
        return {"error": type(e).__name__, "message": str(e)}


def copy_tree(src, dst):
    for base, dirs, files in os.walk(src):
        dirs.sort()
        for name in sorted(files):
            if name == ".lock" or name.startswith("changes"):
                continue
            path = os.path.join(base, name)
            rel = os.path.relpath(path, src)
            os.makedirs(os.path.dirname(os.path.join(dst, rel)), exist_ok=True)
            shutil.copyfile(path, os.path.join(dst, rel))


def run_case(ds, name, batches, tmp, proto):
    rows = [{"ops": ops} for ops in batches]
    result = {"name": name, "batches": rows}
    for fmt in ("dir", "jsonl"):
        root = os.path.join(tmp, name, fmt, "store")
        store = ds.Store(root, fmt=fmt, actor="regen", schema_mode="off",
                         prune_log=os.path.join(tmp, name, fmt, "pruned.jsonl"), clock=lambda: "2026-10-21T10:00:00Z")
        for row, ops in zip(rows, batches):
            row[fmt] = outcome(store, ops)
        out = os.path.join(HERE, name, fmt)
        shutil.rmtree(out, ignore_errors=True)
        copy_tree(root, out)
        if fmt == "dir":
            result["derived"] = derived(proto, ds, root)
    return result


def main(argv):
    ap = argparse.ArgumentParser()
    ap.add_argument("--proto", default=os.environ.get("DASHBOARD_PROTO_DIR"), required=False)
    ap.add_argument("--live", required=True)
    ap.add_argument("--deny-out")
    args = ap.parse_args(argv)
    if not args.proto:
        ap.error("--proto or DASHBOARD_PROTO_DIR is required")
    ds = load_proto(args.proto)
    scrubber = load_scrubber()
    synthetic = synthetic_case(args.proto, scrubber)
    scrubber.ids = {}  # each store gets its own dense id map, as in scrub.py
    cases = [("synthetic", synthetic), ("live", live_case(args.live, scrubber)), ("edge", edge_case())]
    with tempfile.TemporaryDirectory() as tmp:
        corpus = [run_case(ds, name, batches, tmp, args.proto) for name, batches in cases]
    with open(os.path.join(HERE, "corpus.json"), "w", encoding="utf-8") as f:
        f.write(json.dumps({"cases": corpus}, indent=1, sort_keys=True, ensure_ascii=False) + "\n")
    if args.deny_out:
        with open(args.deny_out, "w", encoding="utf-8") as f:
            f.write("".join(d + "\n" for d in sorted(scrubber.deny - scrubber.kept)))


if __name__ == "__main__":
    main(sys.argv[1:])
