#!/usr/bin/env python3
"""relay-marker.py — find submitted relay turns in a PM pane capture.

Why: the double-post guard (scripts/posted.sh) must know which turns in a PM
pane an Architect sent. Relays carry `[<Name> Architect HH:MMZ]`; an Architect
set up before #9997 typed the older `[<Name> supervisor HH:MMZ]`, and a pane's
history keeps it, so both count as relays. Counting more relays only makes the
guard refuse more sends.
What: MARKER matches both forms. The name is optional and the last minute
digit may be `x` (`22:2xZ`). A relay is SUBMITTED when its line sits above the
input box and starts with `❯ ` (typed into the pane) or `› Message from
@<date>: ` (delivered by SendMessage). `find_relays` returns the submitted
relays stamped at or after `since`, minus the caller's own.
Test: tests/test_relay_marker.py, tests/test_relay.py (through posted.sh).
Usage: tmux capture-pane -p -J -t '=<session>:' -S -400 | relay-marker.py <since HH:MMZ> [self-name]
  Prints each relay found and exits 1 (do not send), prints NONE and exits 0
  (send), or exits 2 on a usage error or a malformed time (do not send).
"""
import re
import sys

MARKER = re.compile(
    r"\[(?:(?P<name>[A-Za-z][A-Za-z -]*?) )?(?P<role>Architect|supervisor) "
    r"(?P<hm>\d{2}:\d[\dx])Z\]")
SUBMITTED = re.compile(r"^(?:❯|›\s+Message from @\S+:)\s+(?=\[)")
BOX_TOP_RULE = re.compile(r"^[─━]{10,}(?:\s.{1,30}[─━]+)?$")
# #9997: a `since` that does not parse would compare wrongly and print NONE.
SINCE = re.compile(r"^(?:[01]\d|2[0-3]):[0-5]\dZ?$")


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
    if len(argv) < 2 or not SINCE.match(argv[1]):
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
