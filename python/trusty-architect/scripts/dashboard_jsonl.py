#!/usr/bin/env python3
"""dashboard_jsonl.py: the canonical JSONL encoding of the dashboard store, and its change log (stdlib only).

Why: the owner (2026-10-11 04:12Z): "Make sure all dash data structures are deterministic and encoded in jsonl with a
schema." Same documents must give the same bytes in Python and in the Rust trusty-architect crate, so a file hash
is a state fingerprint and a git diff shows only changed lines.
What: one file per collection, <root>/<coll>.jsonl. One line per document: {"data":{...},"id":"A3e","sv":1,
"version":7}; UTF-8, LF, final newline, no blank lines, lines sorted by id in byte order. Encoding at every depth:
json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False). null and absent are never
normalised. `sv` is the schema version of that line; MIGRATIONS maps N -> a function that returns the line at N+1.
<root>/changes.jsonl is an append-only audit log (seq, ts, actor, op, coll, id, version, sha256 of the line, keys),
rotated monthly to changes-YYYY-MM.jsonl; it is not the source of truth. fsck() checks canonical bytes, order, sv, and
the last logged hash of every document against the file.
Test: tests/test_dashboard_jsonl.py
"""
import hashlib
import json
import os
import re
import sys

SV = 1
MIGRATIONS = {}        # {n: fn(line_dict) -> line_dict at sv n+1}; empty until the first schema bump
CHANGES = "changes.jsonl"
RESERVED = re.compile(r"^changes(-[0-9]{4}-[0-9]{2})?$")


class JsonlError(ValueError):
    """The bytes are not a canonical, readable dashboard collection file."""


def canonical(obj):
    """The one text encoding of a JSON value."""
    try:
        return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)
    except (TypeError, ValueError) as e:
        raise JsonlError(f"not canonically encodable: {e}")


def render_line(doc_id, version, data, sv=SV):
    """One document as bytes, with its trailing LF."""
    text = canonical({"data": data, "id": doc_id, "sv": sv, "version": version}) + "\n"
    try:
        return text.encode("utf-8")
    except UnicodeEncodeError as e:
        raise JsonlError(f"{doc_id}: not valid UTF-8: {e}")


def sort_ids(ids):
    """Byte order of the UTF-8 form, which is code point order and identical in Rust."""
    return sorted(ids, key=lambda s: s.encode("utf-8"))


def render_file(docs):
    """docs: {id: (version, data)} -> bytes ('' for no documents)."""
    return b"".join(render_line(i, docs[i][0], docs[i][1]) for i in sort_ids(docs))


def sha256(blob):
    return hashlib.sha256(blob).hexdigest()


def _upgrade(line, where):
    sv = line.get("sv")
    if not isinstance(sv, int) or isinstance(sv, bool) or sv < 1:
        raise JsonlError(f"{where}: bad sv {sv!r}")
    if sv > SV:
        raise JsonlError(f"{where}: sv {sv} is newer than this reader ({SV})")
    while sv < SV:
        line = MIGRATIONS[sv](line)
        sv += 1
        line["sv"] = sv
    return line


def parse_file(blob, path="<jsonl>"):
    """bytes -> {id: (version, data)}; tolerant of formatting, strict about shape. Use fsck for canonical form."""
    out = {}
    if not blob:
        return out
    try:
        text = blob.decode("utf-8")
    except UnicodeDecodeError as e:
        raise JsonlError(f"{path}: not UTF-8: {e}")
    for n, raw in enumerate(text.split("\n"), 1):
        if raw == "":
            continue
        where = f"{path}:{n}"
        try:
            line = json.loads(raw)
        except ValueError as e:
            raise JsonlError(f"{where}: invalid JSON: {e}")
        if not isinstance(line, dict) or not isinstance(line.get("id"), str) or not isinstance(line.get("data"), dict):
            raise JsonlError(f"{where}: a line is {{data, id, sv, version}}")
        line = _upgrade(line, where)
        version = line.get("version")
        if not isinstance(version, int) or isinstance(version, bool) or version < 1:
            raise JsonlError(f"{where}: bad version {version!r}")
        if line["id"] in out:
            raise JsonlError(f"{where}: duplicate id {line['id']}")
        out[line["id"]] = (version, line["data"])
    return out


def coll_path(root, coll):
    return os.path.join(root, coll + ".jsonl")


def read_coll(root, coll):
    """{id: (version, data)} for <root>/<coll>.jsonl; {} when the file is absent."""
    path = coll_path(root, coll)
    try:
        with open(path, "rb") as f:
            blob = f.read()
    except FileNotFoundError:
        return {}
    return parse_file(blob, path)


def collections(root):
    """Collection names that have a .jsonl file under root (changes logs excluded), sorted."""
    try:
        names = os.listdir(root)
    except FileNotFoundError:
        return []
    return sorted(n[:-6] for n in names if n.endswith(".jsonl") and not n.startswith(".") and not RESERVED.match(n[:-6]))


def fsync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


# ---- change log --------------------------------------------------------------
_TAIL_CHUNK = 8192


def _change_entry(raw):
    """One change-log line -> its entry dict, or None when it is not a JSON object with an integer seq."""
    try:
        entry = json.loads(raw)
    except ValueError:      # also UnicodeDecodeError
        return None
    seq = entry.get("seq") if isinstance(entry, dict) else None
    return entry if isinstance(seq, int) and not isinstance(seq, bool) else None


def _read_tail(path):
    """Why: #10225 — a fixed 8 KiB window and a strict parse of the last line let one torn or oversized line make
    every later append fail, so every later write skipped the audit log.
    What: scans path backwards in chunks to the newest line that is a valid entry, however long. Returns
    (entry or None, torn, skipped): torn is True when the file does not end in LF, skipped counts the lines after
    that entry that are not valid entries. (None, False, 0) for an absent or empty file.
    Test: `test_corrupt_change_log_tail_does_not_block_later_appends`
    """
    try:
        f = open(path, "rb")
    except FileNotFoundError:
        return None, False, 0
    with f:
        pos = f.seek(0, os.SEEK_END)
        if pos == 0:
            return None, False, 0
        f.seek(pos - 1)
        torn = f.read(1) != b"\n"
        carry, skipped = b"", 0
        while pos > 0:
            step = min(_TAIL_CHUNK, pos)
            pos -= step
            f.seek(pos)
            parts = (f.read(step) + carry).split(b"\n")
            carry = parts[0]                # may continue in the previous chunk
            for raw in reversed(parts[1:] if pos else parts):
                if not raw.strip():
                    continue
                entry = _change_entry(raw)
                if entry is not None:
                    return entry, torn, skipped
                skipped += 1
    return None, torn, skipped


def _read_first_entry(path):
    """The first valid entry of path (None when it has none); a corrupt line cannot date the file for rotation."""
    try:
        with open(path, "rb") as f:
            for raw in f:
                entry = _change_entry(raw) if raw.strip() else None
                if entry is not None:
                    return entry
    except FileNotFoundError:
        pass
    return None


def append_changes(root, entries, ts):
    """Append entries (dicts without seq/ts) to changes.jsonl; rotate first when the file is from an earlier month.
    ts is an ISO UTC string. Returns the number written.
    A torn or corrupt tail is not fatal and not trusted (#10225): seq continues from the newest valid entry, a torn
    line is terminated so the new entries start on their own line, the corruption is reported on stderr, and fsck
    keeps reporting the bad line."""
    if not entries:
        return 0
    path = os.path.join(root, CHANGES)
    # #10225: read past a torn/oversized/invalid tail instead of raising on it, which blocked every later append.
    last, torn, skipped = _read_tail(path)
    if torn or skipped:
        sys.stderr.write(f"dashboard-jsonl: {path}: corrupt line(s) at the end of the change log "
                         f"({skipped} invalid, torn={torn}); appending after them, seq continues from "
                         f"{(last or {}).get('seq', 0)}; run fsck\n")
    seq = (last or {}).get("seq", 0)
    first = _read_first_entry(path)
    rotated = bool(first) and str(first.get("ts", ""))[:7] not in ("", ts[:7])
    if rotated:
        os.replace(path, os.path.join(root, f"changes-{str(first['ts'])[:7]}.jsonl"))
    lines = ["\n"] if torn and not rotated else []
    for e in entries:
        seq += 1
        lines.append(canonical(dict(e, seq=seq, ts=ts)) + "\n")
    with open(path, "ab") as f:
        f.write("".join(lines).encode("utf-8"))
        f.flush()
        os.fsync(f.fileno())
    return len(lines)


def changed_keys(old, new):
    """Top-level keys whose value differs between two document bodies (either may be None)."""
    old, new = old or {}, new or {}
    return sorted(k for k in set(old) | set(new) if k not in old or k not in new or canonical(old[k]) != canonical(new[k]))


def _scan_changes(root):
    """(entries, problems) of changes.jsonl: the valid entries in order, and one problem per line that is not one.
    #10225: shared by read_changes (refuses any problem) and fsck (reports each one and checks the rest)."""
    path = os.path.join(root, CHANGES)
    out, problems = [], []
    try:
        with open(path, "rb") as f:
            for n, raw in enumerate(f, 1):
                if not raw.strip():
                    continue
                entry = _change_entry(raw)
                if entry is None:
                    problems.append(f"{path}:{n}: invalid change line (torn, not JSON, or no integer seq)")
                else:
                    out.append(entry)
    except FileNotFoundError:
        pass
    return out, problems


def read_changes(root):
    """All entries of changes.jsonl in order (rotated months are not read); JsonlError on a corrupt line."""
    out, problems = _scan_changes(root)
    if problems:
        raise JsonlError(problems[0])
    return out


def fsck(root):
    """Problems (list of strings) in the jsonl store at root: non-canonical bytes, unsorted lines, bad ids,
    and any document whose last logged hash differs from its line (or that was logged deleted but exists)."""
    problems = []
    lines = {}
    for coll in collections(root):
        path = coll_path(root, coll)
        with open(path, "rb") as f:
            blob = f.read()
        try:
            docs = parse_file(blob, path)
        except JsonlError as e:
            problems.append(str(e))
            continue
        if blob != render_file(docs):
            problems.append(f"{path}: bytes are not canonical (re-render differs)")
        for doc_id, (version, data) in docs.items():
            lines[(coll, doc_id)] = sha256(render_line(doc_id, version, data))
    last = {}
    changes, bad = _scan_changes(root)
    problems.extend(bad)
    for e in changes:
        last[(e.get("coll"), e.get("id"))] = e
    for key, e in sorted(last.items(), key=lambda kv: (str(kv[0][0]), str(kv[0][1]))):
        want = e.get("sha256")
        have = lines.get(key)
        if e.get("op") == "delete":
            if have is not None:
                problems.append(f"{key[0]}/{key[1]}: logged deleted (seq {e.get('seq')}) but present in the file")
        elif have != want:
            problems.append(f"{key[0]}/{key[1]}: file line hash differs from last logged hash (seq {e.get('seq')})")
    return problems


def main(argv=None):
    root = (argv or sys.argv[1:] or ["."])[0]
    probs = fsck(root)
    for p in probs:
        print(p)
    return 1 if probs else 0


if __name__ == "__main__":
    sys.exit(main())
