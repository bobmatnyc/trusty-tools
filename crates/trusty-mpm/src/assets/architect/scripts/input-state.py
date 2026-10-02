#!/usr/bin/env python3
"""Classify a Claude Code pane's input box as empty, suggestion, or typed.

Why: Claude Code renders an auto-generated next-prompt suggestion as dim text
(SGR 2). A plain capture shows it exactly like text a human typed, and the
Architect must never submit or overwrite a real draft.
What: for each pane id argument, prints "<pane> <state> | <text>".
`classify(capture)` is the importable classifier (scripts/fleet-poll.py uses it).
Test: run against a pane showing a dim suggestion and against one with typed
text; tests/test_fleet_poll.py covers `classify` with fixture captures.
Usage: scripts/input-state.py %261 %265 ...
"""
import os
import re
import subprocess
import sys

# Fleet tmux socket. Empty means tmux's own default resolution ($TMUX inside a
# session, else /tmp/tmux-<uid>/default).
SOCKET = os.environ.get("TMUX_SOCKET", "")
PROMPT = "❯"  # the ❯ glyph Claude Code draws at the input line
SGR = re.compile(r"\x1b\[[0-9;]*m")


def classify(capture):
    """Return (state, text) for a capture taken with escape codes (-e).

    state is "no-prompt", "empty", "suggestion" (dim SGR 2) or "typed"; text is
    the input line with escapes removed. The last ❯ line is the input box.
    """
    lines = [l for l in capture.splitlines() if PROMPT in l]
    if not lines:
        return "no-prompt", ""
    raw = lines[-1].split(PROMPT, 1)[1]
    text = SGR.sub("", raw).strip()
    if not text:
        return "empty", text
    if "\x1b[2m" in raw:
        return "suggestion", text
    return "typed", text


def main(panes):
    for pane in panes:
        try:
            out = subprocess.run(
                ["tmux", *(["-S", SOCKET] if SOCKET else []),
                 "capture-pane", "-t", pane, "-p", "-e"],
                capture_output=True, text=True, check=True,
            ).stdout
        except subprocess.CalledProcessError:
            print(f"{pane} missing |")
            continue
        state, text = classify(out)
        if state == "no-prompt":
            print(f"{pane} no-prompt |")
            continue
        print(f"{pane} {state} | {text[:100]}")


if __name__ == "__main__":
    main(sys.argv[1:])
