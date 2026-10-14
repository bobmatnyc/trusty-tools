#!/usr/bin/env python3
"""relay-marker.py — find submitted relay turns in a PM pane capture.

Why: the double-post guard (posted.sh) must know which turns in a PM pane a
supervisor sent. The Architect rename (docs/specs/SPEC-ARCHITECT-01.md, A)
adds a second marker form; PM transcripts keep the first, so both forms stay
valid indefinitely.
What: MARKER matches `[<Name> supervisor HH:MMZ]` and `[<Name> Architect
HH:MMZ]`. The name is optional and the last minute digit may be `x`
(`22:2xZ`), both seen in live panes. A relay is SUBMITTED when its line sits
above the input box and starts with `❯ ` (typed into the pane) or
`› Message from @<date>: ` (delivered by SendMessage). `find_relays` returns
the submitted relays stamped at or after `since`, minus the caller's own.
Test: tests/test_relay_marker.py.
Usage: tmux capture-pane -p -J -t <target> -S -400 | relay-marker.py <since HH:MMZ> [self-name]
  Prints each relay found and exits 1 (do not send), or prints NONE and exits 0.
"""
import re
import sys

MARKER = re.compile(
    r"\[(?:(?P<name>[A-Za-z][A-Za-z -]*?) )?(?P<role>supervisor|Architect) "
    r"(?P<hm>\d{2}:\d[\dx])Z\]")
SUBMITTED = re.compile(r"^(?:❯|›\s+Message from @\S+:)\s+(?=\[)")
BOX_TOP_RULE = re.compile(r"^[─━]{10,}(?:\s.{1,30}[─━]+)?$")


def box_start(lines):
    """Index of the input box line (the last ❯ line directly under a rule), or len(lines)."""
    for i in range(len(lines) - 1, 0, -1):
        if lines[i].startswith("❯") and BOX_TOP_RULE.match(lines[i - 1].strip()):
            return i
    return len(lines)


def relay_marker(line):
    """The MARKER match of a submitted relay line, or None."""
    m = SUBMITTED.match(line)
    return MARKER.match(line, m.end()) if m else None


def is_own(match, self_name):
    """True when the marker names the caller. A marker with no name is never own."""
    name = (match.group("name") or "").strip().lower()
    me = self_name.strip().lower()
    return bool(me) and bool(name) and (name == me or name.startswith(me + " "))


def find_relays(lines, since, self_name=""):
    """Submitted relay lines above the input box stamped at or after `since` (HH:MM[Z])."""
    since = since.rstrip("Z")
    hits = []
    for line in lines[:box_start(lines)]:
        m = relay_marker(line)
        if m and m.group("hm") >= since and not is_own(m, self_name):
            hits.append(line)
    return hits


def main(argv):
    if len(argv) < 2:
        print("usage: relay-marker.py <since HH:MMZ> [self-name]", file=sys.stderr)
        return 2
    hits = find_relays(sys.stdin.read().splitlines(), argv[1], argv[2] if len(argv) > 2 else "")
    if hits:
        print("\n".join(h[:160] for h in hits))
        return 1
    print("NONE")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
