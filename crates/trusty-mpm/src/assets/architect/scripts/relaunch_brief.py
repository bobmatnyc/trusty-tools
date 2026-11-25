#!/usr/bin/env python3
"""relaunch_brief.py: the one relaunch-resume brief and the per-session text rendered from it.

Why: session-relaunch.sh --il wrote local/briefs/relaunch-resume-<session>.md per PM (30 near-identical files on the
dashboard briefs page). Owner ruling (see #10571): "similar briefs should be edited, not duplicated, use local git to
track changes." The text now lives once, in local/briefs/relaunch-resume.md, with the per-session ledger as a parameter.
What: TEMPLATES holds the two resume texts (datg and dbatg ledger layouts). render() fills the placeholders at send time;
the result is byte-for-byte the body the old per-session brief held. sync(path) rewrites the brief only when its body
differs, keeping the file's own header (brief-id marker and H1) so a stamped id survives; a missing file is created.
Usage: relaunch_brief.py sync <brief-path>
       relaunch_brief.py render datg <digest-command>
       relaunch_brief.py render dbatg <ledger-dir> [<cron-prompts-path>]
render prints the text with no trailing newline (the caller types it as one line).
Test: tests/test_relaunch_brief.py, tests/test_session_relaunch_il.py
"""
import os
import sys

DIGEST = "{DIGEST_COMMAND}"
LEDGER = "{LEDGER_DIR}"
CRONS = "{CRON_PROMPTS_SENTENCE}"
CRONS_SENTENCE = " The full cron prompts are in {path}."

TEMPLATES = {
    "datg": ("You were relaunched onto the new Claude account. Resume from your Intent Ledger: run " + DIGEST
             + "; do not run session_context_catchup and do not run /tm-session-resume. Session crons died with the old "
             "process: re-create each cron listed in the digest once, then continue the work in next."),
    "dbatg": ("You were relaunched onto the new Claude account. Resume from your Intent Ledger: read its digest with your "
              "dbatg MCP tool (ledger folder " + LEDGER + "); do not use datg.py, do not run session_context_catchup and "
              "do not run /tm-session-resume. Session crons died with the old process: re-create each cron listed in the "
              "digest once." + CRONS + " Then continue the work in next."),
}

TITLE = "# PM relaunch resume (session-relaunch.sh --il)"

BODY = f"""
One brief for every PM that `scripts/session-relaunch.sh --il` relaunches onto a new Claude account. The script fills
the placeholders for that session and types the text into its pane after the relay marker
(`[<Name> Architect HH:MMZ] Architect answer: `, <Name> from local/architect.json). The text lives in `scripts/relaunch_brief.py`; a rerun rewrites this
file only when that text changes, and git keeps the history (the owner: "similar briefs should be edited, not
duplicated, use local git to track changes.").

## Parameters

- `{DIGEST}`: `python3 <architect>/scripts/datg.py --project <project-dir> digest`, with
  ` --session <name>` before `digest` when the relaunch passed `--il-session <name>`.
- `{LEDGER}`: the session's dbatg ledger folder, `<project-dir>/.trusty-mpm/dbatg/<uuid or name>`.
- `{CRONS}`: empty, or ` The full cron prompts are in <ledger>/cron-prompts.md.` when that file exists.

## Text for a datg ledger

```text
{TEMPLATES["datg"]}
```

## Text for a dbatg ledger

```text
{TEMPLATES["dbatg"]}
```
"""


def render(kind, value, crons_path=""):
    """The resume text for one session: the template with its placeholders filled (str.replace, so paths may hold braces)."""
    if kind == "datg":
        return TEMPLATES["datg"].replace(DIGEST, value)
    if kind == "dbatg":
        sentence = CRONS_SENTENCE.format(path=crons_path) if crons_path else ""
        return TEMPLATES["dbatg"].replace(LEDGER, value).replace(CRONS, sentence)
    raise ValueError(f"unknown ledger kind: {kind}")


def wanted(existing):
    """Full brief text: the existing header (everything through the first H1 line) or TITLE, then BODY."""
    lines = existing.split("\n") if existing else []
    for i, ln in enumerate(lines):
        if ln.startswith("# "):
            return "\n".join(lines[:i + 1]) + "\n" + BODY
    return TITLE + "\n" + BODY


def sync(path):
    """Write the brief only when its text differs. Returns True when the file was written."""
    try:
        with open(path, encoding="utf-8") as f:
            have = f.read()
    except FileNotFoundError:
        have = None
    want = wanted(have)
    if have == want:
        return False
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    tmp = f"{path}.tmp.{os.getpid()}"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(want)
    os.replace(tmp, path)
    return True


def main(argv):
    if len(argv) == 2 and argv[0] == "sync":
        sync(argv[1])
        return 0
    if len(argv) in (3, 4) and argv[0] == "render" and argv[1] in TEMPLATES and (argv[1] == "dbatg" or len(argv) == 3):
        sys.stdout.write(render(argv[1], argv[2], argv[3] if len(argv) == 4 else ""))
        return 0
    print(__doc__.split("Usage: ", 1)[1].split("render prints", 1)[0].rstrip(), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
