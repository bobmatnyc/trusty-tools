#!/usr/bin/env python3
"""check_owner_scenarios.py — the owner-scenario lint (#10576).

Why: a click-by-click scenario form made a month of owner work and would have
  frozen the screens. An owner scenario states an outcome, not a UI script, and
  traces to the team's tests. `tm-owner-scenarios` states the form; this lint
  checks it, so the PM and CI can run the same check.

What: reads one or more Markdown scenario files and a test matrix. A scenario
  is a heading block (a `#` line through the next heading) that holds a field
  label or a numbered list item. The file's first heading, when it is a
  level-1 title, may carry intro prose. Any other heading block that has body
  text but is not a scenario fails (never skipped silently). Each scenario must:
    - carry non-empty `Intent:`, `Situation:`, `Result:` and `Trace:` lines
      (a list bullet and `**bold**` labels are accepted);
    - list at least one test ID on `Trace:`, each present as a whole word in
      the matrix file;
    - hold no run of two or more numbered steps that start with a UI verb
      (click, tap, press, select, type, open, drag, scroll, enter, choose,
      swipe, hover, fill, submit), on one line or across lines.
  Length is never checked. A file with no scenario fails (scan floor).

Usage: check_owner_scenarios.py --matrix <matrix.md> <scenarios.md>...
Exit:  0 every scenario passes; 1 a finding (each printed as
       `<file>:<line>: <heading>: <reason>`); 2 a usage or read error.
Test:  scripts/check_owner_scenarios_selftest.sh.
"""

import argparse
import re
import sys
from pathlib import Path

FIELDS = ("Intent", "Situation", "Result", "Trace")
HEADING_RE = re.compile(r"^\s{0,3}#{1,6}\s+(.*?)\s*#*\s*$")
FIELD_RE = re.compile(
    r"^\s*(?:[-*+]\s+)?(?:\*\*|__)?(" + "|".join(FIELDS) + r")(?:\*\*|__)?\s*:\s*(?:\*\*|__)?\s*(.*)$"
)
NUMBERED_ITEM_RE = re.compile(r"^\s*\d+[.)]\s+")
UI_VERBS = "click|tap|press|select|type|open|drag|scroll|enter|choose|swipe|hover|fill|submit"
UI_STEP_RE = re.compile(r"(?:^|(?<=\s))\d+[.)]\s+[*_`]*(?:" + UI_VERBS + r")\b", re.IGNORECASE)
TRACE_SPLIT_RE = re.compile(r"[,;\s]+")


class Scenario:
    def __init__(self, heading, line, level):
        self.heading = heading
        self.line = line
        self.level = level
        self.body_lines = 0
        self.fields = {}
        self.ui_steps = 0
        self.numbered = False

    def is_scenario(self):
        return bool(self.fields) or self.numbered


def parse_blocks(text):
    """Split `text` into heading blocks and record fields and numbered steps."""
    blocks, current = [], None
    for lineno, raw in enumerate(text.splitlines(), start=1):
        heading = HEADING_RE.match(raw)
        if heading:
            level = len(raw.lstrip()) - len(raw.lstrip().lstrip("#"))
            current = Scenario(heading.group(1), lineno, level)
            blocks.append(current)
            continue
        if current is None:
            continue
        if raw.strip():
            current.body_lines += 1
        field = FIELD_RE.match(raw)
        if field and field.group(1) not in current.fields:
            current.fields[field.group(1)] = field.group(2).strip().rstrip("*_").strip()
        if NUMBERED_ITEM_RE.match(raw):
            current.numbered = True
        current.ui_steps += len(UI_STEP_RE.findall(raw))
    return blocks


def parse(text):
    """Return the scenario blocks of `text`."""
    return [b for b in parse_blocks(text) if b.is_scenario()]


def orphan_blocks(text):
    """Return non-scenario blocks with body text, other than a leading `#` title."""
    blocks = parse_blocks(text)
    orphans = []
    for index, block in enumerate(blocks):
        if block.is_scenario() or not block.body_lines:
            continue
        if index == 0 and block.level == 1:
            continue
        orphans.append(block)
    return orphans


def trace_ids(value):
    return [t.strip("`*_.()[]") for t in TRACE_SPLIT_RE.split(value) if t.strip("`*_.()[]")]


def in_matrix(test_id, matrix):
    pattern = r"(?<![A-Za-z0-9_-])" + re.escape(test_id) + r"(?![A-Za-z0-9_-])"
    return re.search(pattern, matrix) is not None


def lint(path, text, matrix):
    findings = []
    scenarios = parse(text)
    for o in orphan_blocks(text):
        findings.append(
            f"{path}:{o.line}: {o.heading}: heading block has body text but no scenario "
            "fields or numbered list; make it a scenario or remove it"
        )
    if not scenarios:
        return findings + [f"{path}:1: no scenario found (scan floor): a scenario is a heading block with Intent/Situation/Result/Trace"]
    for s in scenarios:
        where = f"{path}:{s.line}: {s.heading}"
        for name in FIELDS:
            if not s.fields.get(name):
                findings.append(f"{where}: missing or empty `{name}:`")
        ids = trace_ids(s.fields.get("Trace", ""))
        for test_id in ids:
            if not in_matrix(test_id, matrix):
                findings.append(f"{where}: Trace ID `{test_id}` is not in the test matrix")
        if s.ui_steps >= 2:
            findings.append(f"{where}: {s.ui_steps} numbered UI steps; state the outcome, not the clicks")
    return findings


def main(argv):
    parser = argparse.ArgumentParser(description="Lint owner scenarios (#10576).")
    parser.add_argument("--matrix", required=True, help="test matrix file holding the test IDs")
    parser.add_argument("files", nargs="+", help="scenario Markdown files")
    args = parser.parse_args(argv)
    try:
        matrix = Path(args.matrix).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as err:
        print(f"check_owner_scenarios: cannot read matrix {args.matrix}: {err}", file=sys.stderr)
        return 2
    findings, count = [], 0
    for name in args.files:
        try:
            text = Path(name).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as err:
            print(f"check_owner_scenarios: cannot read {name}: {err}", file=sys.stderr)
            return 2
        count += len(parse(text))
        findings.extend(lint(name, text, matrix))
    for finding in findings:
        print(finding)
    if findings:
        print(f"check_owner_scenarios: {len(findings)} finding(s) in {len(args.files)} file(s).")
        return 1
    print(f"check_owner_scenarios: {count} scenario(s) in {len(args.files)} file(s) — OK.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
