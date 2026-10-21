#!/usr/bin/env python3
"""Write scrubbed PM-ledger goldens from live `datg` stores (#10062).

Why: the Rust ledger types must round-trip the prototype's on-disk bytes, but
a live store holds owner-ruling ids, session names, paths, PR numbers, SHAs
and titles. Only scrubbed copies may be committed (#9756 plan section 3).
What: for each `NAME=STORE_DIR` argument, reads `state.json` and the first
lines of `archive.jsonl` and `log.jsonl`, and writes `NAME/` beside this file
with every free-text string replaced by filler of the same length (words from
FILLER), every ledger code by a synthetic `x?` code, every id number by a
dense `01..n` per (code, prefix) in first-seen order (one map per store for
`id`, `goal`, `maps` and log and archive ids; `next` becomes n + 1), and every
log session name by `pm-<n>`. Enum values, timestamps, numbers and key order are kept, so
the shape and the size bar class are kept. Output is Python's compact
`json.dumps(..., ensure_ascii=False)`, the prototype's own writer form.
`--deny-out PATH` (outside the repository) writes one real value per line;
`ledger_fixtures_carry_no_denied_value` reads it through
TRUSTY_LEDGER_DENYLIST. Deterministic: the same stores give the same bytes.
Test: `ledger_fixtures_are_scrubbed`, `ledger_fixture_ids_are_dense_per_prefix`.
"""
import json
import os
import re
import sys

FILLER = ("lorem", "ipsum", "dolor", "sit", "amet", "elit", "sed", "tempor", "über", "magna")
KEEP = {"status", "kind", "who", "mode", "esc", "ev"}
IDISH = re.compile(r"^(?:([a-z][a-z0-9]{1,7})-)?([A-Z][0-9a-f]{2}|init|session)$")
TS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?Z$")
SAMPLE = 8


class Scrubber:
    def __init__(self):
        self.codes, self.sessions, self.deny, self.kept = {}, {}, set(), set()
        self.ids = {}  # per store: (real code, prefix) -> {real hex: new number}

    def code(self, real):
        if real not in self.codes:
            self.codes[real] = "x" + "abcdefghijklmnopqrstuvwxyz"[len(self.codes)]
            self.deny.add(real + "-")
        return self.codes[real]

    def session(self, real):
        if real not in self.sessions:
            self.sessions[real] = f"pm-{len(self.sessions) + 1}"
            self.deny.add(real)
        return self.sessions[real]

    def filler(self, text, sep=" ", ascii_only=False):
        words = [w for w in FILLER if not ascii_only or w.isascii()]
        out, i = [], 0
        for line in text.split("\n"):
            buf = ""
            while len(buf) < len(line):
                buf += (sep if buf else "") + words[i % len(words)]
                i += 1
            out.append(buf[: len(line)])
        if len(text) >= 8:
            self.deny.add(text)
        return "\n".join(out)

    def value(self, key, v):
        if isinstance(v, dict):
            return {k: self.value(k, x) for k, x in v.items()}
        if isinstance(v, list):
            return [self.value(key, x) for x in v]
        if not isinstance(v, str) or v == "" or key in KEEP or TS.match(v):
            self.kept.add(v)
            return v
        if key == "code":
            return self.code(v)
        if key == "session":
            return self.session(v)
        if key == "key":
            return self.filler(v, "-", True)
        m = IDISH.match(v) if key in ("id", "goal", "maps") else None
        if m:
            tail = m.group(2)
            if tail not in ("init", "session"):
                nums = self.ids.setdefault((m.group(1) or "", tail[0]), {})
                tail = f"{tail[0]}{nums.setdefault(tail[1:], len(nums) + 1):02x}"
            return (self.code(m.group(1)) + "-" if m.group(1) else "") + tail
        if key in ("G", "D", "A", "T", "B"):
            return v  # `next` counters: two hex digits
        return self.filler(v)


def dump(obj):
    return json.dumps(obj, separators=(",", ":"), ensure_ascii=False) + "\n"


def head(path, n):
    try:
        with open(path, encoding="utf-8") as f:
            return [json.loads(ln) for ln in f.read().splitlines()[:n]]
    except FileNotFoundError:
        return []


def main(argv):
    deny_out = None
    if argv[:1] == ["--deny-out"]:
        deny_out, argv = argv[1], argv[2:]
    s = Scrubber()
    here = os.path.dirname(os.path.abspath(__file__))
    for arg in argv:
        name, src = arg.split("=", 1)
        with open(os.path.join(src, "state.json"), encoding="utf-8") as f:
            st = json.load(f)
        s.code(st["code"])
        s.ids = {}
        out = os.path.join(here, name)
        os.makedirs(out, exist_ok=True)
        scrubbed, logs = s.value("", st), {}
        for log in ("archive.jsonl", "log.jsonl"):
            lines = head(os.path.join(src, log), SAMPLE)
            if log == "log.jsonl":  # keep both log shapes: t/ev and ts/kind
                lines += [x for x in head(os.path.join(src, log), 10**6) if "kind" in x][:SAMPLE // 2]
            if lines:
                logs[log] = "".join(dump(s.value("", x)) for x in lines)
        for prefix in scrubbed["next"]:
            scrubbed["next"][prefix] = f"{len(s.ids.get((st['code'], prefix), {})) + 1:02x}"
        logs["state.json"] = dump(scrubbed)
        for log, text in logs.items():
            with open(os.path.join(out, log), "w", encoding="utf-8") as f:
                f.write(text)
    if deny_out:
        with open(deny_out, "w", encoding="utf-8") as f:
            f.write("".join(d + "\n" for d in sorted(s.deny - s.kept)))


if __name__ == "__main__":
    main(sys.argv[1:])
