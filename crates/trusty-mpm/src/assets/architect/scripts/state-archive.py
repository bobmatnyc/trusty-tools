#!/usr/bin/env python3
"""state-archive.py: move old handoff/UPDATE sections out of local/state.md into local/state-archive.md.

Why: state.md grew to 1.29 MB of stacked handoff sections; an accidental full read costs ~330k tokens
(T09 round 1, change 8).
What: a section is a "## " heading through the next "## " heading. A section is a handoff section when
its heading contains "handoff" or "UPDATE" (case-sensitive UPDATE). The first --keep (default 3) of
them in file order (newest first) stay; the rest move, in file order, to the TOP of the archive body
(the archive stays newest first). Every other section stays, so NEXT IDS, poll_count and
last_consumed_ts are untouched. Refuses if the move would remove the only NEXT IDS line.
--dry-run prints what would move and writes nothing.
Test: scripts/test_state_archive.py
"""
import argparse
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NEXT_IDS_RE = re.compile(r"NEXT IDS:?\s+[GPADTS]\s*[0-9a-f]{2}")
ARCHIVE_HEAD = "# State archive (handoff sections moved out of state.md by scripts/state-archive.py; newest first)\n"


def split_sections(text):
    """Return (preamble, [section text, ...]); each section starts at a '## ' line."""
    lines = text.splitlines(keepends=True)
    idx = [i for i, ln in enumerate(lines) if ln.startswith("## ")]
    if not idx:
        return text, []
    pre = "".join(lines[:idx[0]])
    secs = ["".join(lines[a:b]) for a, b in zip(idx, idx[1:] + [len(lines)])]
    return pre, secs


def is_handoff(section):
    head = section.splitlines()[0]
    return "handoff" in head.lower() or "UPDATE" in head


def run(state_path, archive_path, keep=3, dry_run=False):
    with open(state_path, encoding="utf-8") as f:
        text = f.read()
    pre, secs = split_sections(text)
    seen, keep_secs, move = 0, [], []
    for s in secs:
        if is_handoff(s):
            seen += 1
            (keep_secs if seen <= keep else move).append(s)
        else:
            keep_secs.append(s)
    # keep_secs lost the original interleaving for handoffs vs others only if reordered; rebuild in order
    moved_ids = {id(s) for s in move}
    new_state = pre + "".join(s for s in secs if id(s) not in moved_ids)
    if NEXT_IDS_RE.search(text) and not NEXT_IDS_RE.search(new_state):
        sys.exit("state-archive: refusing, the move would remove the only NEXT IDS line")
    if dry_run:
        for s in move:
            print(f"would move {len(s.encode())} bytes: {s.splitlines()[0][:100]}")
        print(f"would move {len(move)} sections; state.md {len(text.encode())} -> {len(new_state.encode())} bytes")
        return len(move)
    if not move:
        return 0
    try:
        with open(archive_path, encoding="utf-8") as f:
            old = f.read()
    except FileNotFoundError:
        old = ""
    body = old[len(ARCHIVE_HEAD):] if old.startswith(ARCHIVE_HEAD) else old
    with open(archive_path, "w", encoding="utf-8") as f:
        f.write(ARCHIVE_HEAD + "".join(move) + body)
    with open(state_path, "w", encoding="utf-8") as f:
        f.write(new_state)
    return len(move)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--state", default=os.path.join(ROOT, "local", "state.md"))
    ap.add_argument("--archive", default=os.path.join(ROOT, "local", "state-archive.md"))
    ap.add_argument("--keep", type=int, default=3)
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args(argv)
    n = run(a.state, a.archive, a.keep, a.dry_run)
    if not a.dry_run:
        print(f"moved {n} sections to {a.archive}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
