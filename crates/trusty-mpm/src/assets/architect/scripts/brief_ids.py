#!/usr/bin/env python3
"""brief_ids.py: durable brief ids (the owner 2026-10-28 17:2xZ: "Briefs need IDs. They're longer lasting, so 4 hex digits").

Why: dashboard and IL ids cycle; a brief is cited for months, so its id must never be reused.
What: a new id is "MB" + 4 lowercase hex digits (MB0000-MBffff; the owner 2026-10-28, D58: "use MB for briefs (Media:Briefs)"). Older ids "Bxxxx" stay in
the registry and files unchanged and are read as the same number (the page shows them as MBxxxx). A brief file carries the marker `<!-- brief-id: MBxxxx -->` as its
first line (after a leading YAML frontmatter block, if any) and its first H1 starts with "MBxxxx: ". The append-only registry
local/briefs/registry.jsonl holds {id, path, title, created, project}. allocate() returns one more than the highest id in the
registry or in the dashboard media store (media docs keep the B space; the number is shared), so an id is never reused and never collides.
Test: scripts/test_brief_id.py
"""
import json
import os
import re
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
BRIEFS = os.path.join(REPO, "local", "briefs")
REGISTRY = os.path.join(BRIEFS, "registry.jsonl")
MEDIA_DIR = os.path.join(REPO, "local", "owner-queue", "media")
SPACE = 1 << 16
ID_RE = re.compile(r"^B[0-9a-f]{4}$")          # media doc ids (and legacy brief ids)
BRIEF_ID_RE = re.compile(r"^M?B[0-9a-f]{4}$")   # brief ids: legacy B or current MB
MARKER_RE = re.compile(r"^<!--\s*brief-id:\s*(M?B[0-9a-f]{4})\s*-->\s*$")
FRONT_RE = re.compile(r"\A---\n.*?\n---\n", re.S)
H1_RE = re.compile(r"^#\s+(.*?)\s*$")
H1_ID_RE = re.compile(r"^(M?B[0-9a-f]{4}):\s+")


def read_registry(registry=REGISTRY):
    rows = []
    if os.path.isfile(registry):
        with open(registry, encoding="utf-8") as f:
            for line in f:
                if line.strip():
                    rows.append(json.loads(line))
    return rows


def media_ids(media_dir=MEDIA_DIR):
    if not os.path.isdir(media_dir):
        return set()
    return {int(n[1:5], 16) for n in os.listdir(media_dir) if n.endswith(".json") and ID_RE.match(n[:-5])}


def number(bid):
    """The 4-hex number of a brief id, B or MB form."""
    return int(bid[-4:], 16)


def used(registry=REGISTRY, media_dir=MEDIA_DIR):
    return {number(r["id"]) for r in read_registry(registry) if BRIEF_ID_RE.match(r["id"])} | media_ids(media_dir)


def allocate(registry=REGISTRY, media_dir=MEDIA_DIR):
    """Next id: highest used + 1. Raises when MBffff is taken (ids never wrap)."""
    u = used(registry, media_dir)
    n = (max(u) + 1) if u else 0
    if n >= SPACE:
        raise RuntimeError("brief ids exhausted (MBffff reached); ids never wrap")
    return f"MB{n:04x}"


def _split_front(text):
    m = FRONT_RE.match(text)
    return (text[:m.end()], text[m.end():]) if m else ("", text)


def marker_id(text):
    """The brief id in text's marker line (first line after optional frontmatter), or None."""
    _, body = _split_front(text)
    m = MARKER_RE.match(body.split("\n", 1)[0])
    return m.group(1) if m else None


def _h1_index(lines):
    fence = False
    for i, ln in enumerate(lines):
        if ln.lstrip().startswith(("```", "~~~")):
            fence = not fence
        elif not fence and H1_RE.match(ln):
            return i
    return None


def title_of(text, path=""):
    """First H1 text without a leading id, or a title from the file name."""
    _, body = _split_front(text)
    lines = body.split("\n")
    i = _h1_index(lines)
    if i is not None:
        return H1_ID_RE.sub("", H1_RE.match(lines[i]).group(1), count=1)
    return slug_title(path)


def slug_title(path):
    name = re.sub(r"^\d{4}-\d{2}-\d{2}-", "", os.path.splitext(os.path.basename(path))[0])
    return name.replace("-", " ").replace("_", " ").strip().capitalize() or "Brief"


def stamp(text, bid, path=""):
    """text with the marker and the "Bid: " H1 prefix applied (idempotent for the same id)."""
    front, body = _split_front(text)
    lines = body.split("\n")
    if MARKER_RE.match(lines[0]):
        lines = lines[1:]
    i = _h1_index(lines)
    if i is None:
        lines = [f"# {bid}: {slug_title(path)}", ""] + lines
    else:
        h = H1_RE.match(lines[i]).group(1)
        lines[i] = f"# {bid}: {H1_ID_RE.sub('', h, count=1)}"
    return front + f"<!-- brief-id: {bid} -->\n" + "\n".join(lines)


def _now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _rel(path):
    ap = os.path.abspath(path)
    return os.path.relpath(ap, REPO) if ap.startswith(REPO + os.sep) else ap


def assign(path, project="architect", registry=REGISTRY, media_dir=MEDIA_DIR):
    """Give the file a brief id (keep it if already marked). Returns (id, created_bool)."""
    with open(path, encoding="utf-8") as f:
        text = f.read()
    have = marker_id(text)
    rel = _rel(path)
    if have:
        if not any(r["id"] == have for r in read_registry(registry)):
            _append(registry, {"id": have, "path": rel, "title": title_of(text, path), "created": _now(), "project": project})
        return have, False
    bid = allocate(registry, media_dir)
    new = stamp(text, bid, path)
    with open(path, "w", encoding="utf-8") as f:
        f.write(new)
    _append(registry, {"id": bid, "path": rel, "title": title_of(new, path), "created": _now(), "project": project})
    return bid, True


def _append(registry, row):
    os.makedirs(os.path.dirname(registry), exist_ok=True)
    with open(registry, "a", encoding="utf-8") as f:
        f.write(json.dumps(row, ensure_ascii=False) + "\n")


def local_briefs(briefs=BRIEFS):
    return sorted(os.path.join(briefs, n) for n in os.listdir(briefs) if n.endswith(".md") and os.path.isfile(os.path.join(briefs, n)))


def check(briefs=BRIEFS, registry=REGISTRY):
    """List of problem strings; empty when every local brief has an id and registry and files agree."""
    probs, rows = [], read_registry(registry)
    by_id, seen = {}, {}
    for r in rows:
        if r["id"] in by_id:
            probs.append(f"duplicate registry id {r['id']}")
        by_id[r["id"]] = r
        if r["path"] in seen and seen[r["path"]] != r["id"]:
            probs.append(f"path {r['path']} has two ids {seen[r['path']]} and {r['id']}")
        seen[r["path"]] = r["id"]
    ids_in_files = {}
    for p in local_briefs(briefs):
        with open(p, encoding="utf-8") as f:
            text = f.read()
        bid, rel = marker_id(text), _rel(p)
        if not bid:
            probs.append(f"no brief id: {rel}")
            continue
        if bid in ids_in_files:
            probs.append(f"duplicate id {bid}: {ids_in_files[bid]} and {rel}")
        ids_in_files[bid] = rel
        if bid not in by_id:
            probs.append(f"{bid} ({rel}) not in registry")
        elif by_id[bid]["path"] != rel:
            probs.append(f"{bid} registry path {by_id[bid]['path']} != file {rel}")
        _, body = _split_front(text)
        lines = body.split("\n")
        i = _h1_index(lines)
        if i is None or not lines[i].startswith(f"# {bid}: "):
            probs.append(f"{bid} ({rel}) H1 does not start with '{bid}: '")
    return probs
