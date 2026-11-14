#!/usr/bin/env python3
"""level_lint.py: warn when owner-facing text carries detail the technical level does not allow (#11064, TL-11 to TL-13).

Why: the level setting (technical_level.py) only helps if text at pm or below stays free of paths, SHAs, flags and
tool names; a format check catches the mechanical part. Vocabulary stays a sampled review (TL-12). The owner ruled
that a violation warns and never blocks (#11064), so nothing here refuses a write.
What: lint(text, level) returns a list of {"rule", "match", "line"} at non-technical, vibe-coder and pm (an unset or
unknown level reads as pm), else []. Rules: path (a path with a file extension, optionally :line or :line-line), sha
(7-40 hex chars with a digit and a letter), flag (--x), envvar (UPPER_SNAKE), job (a quoted name after "job" or
"workflow"), tool (cargo, pytest, tmux, ...), identifier (any backticked span not already flagged). URLs and issue or
PR refs (#123, owner/repo#123) are allowed. warn_item(store, doc_id, data) lints an item write's title, detail and
options at the item's level and prints each warning to stderr; it never raises, so dashboard-db.py calls it on every
item create, set, update and batch entry without a lint fault reaching the write.
CLI: level_lint.py [--level L] [--strict] [--file F | TEXT...]; level defaults to the stored setting. Warnings go to
stdout and the exit code is 0; --strict exits 1 when there is any warning (tests).
Ported from the architect prototype, commit ab5a222 (scripts/level_lint.py); warn_item is the prototype's
dashboard-db.py warn_level, moved here.
Test: tests/test_technical_level.py
"""
import argparse
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import technical_level as tl  # noqa: E402

# #11064: the levels the lint applies at; engineer and above may carry paths, SHAs and flags.
LINTED = tl.LEVELS[:tl.LEVELS.index("engineer")]
_URL = re.compile(r"https?://\S+")
_EXT = r"(?:py|rs|md|json|jsonl|toml|ya?ml|sh|js|ts|tsx|html|css|txt|lock|cfg|ini|sql|log)"
_RULES = (
    ("path", re.compile(rf"(?<![\w])(?:[\w.~-]+/)*[\w.-]+\.{_EXT}(?::\d+(?:-\d+)?)?(?![\w])")),
    ("sha", re.compile(r"(?<![\w-])(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,40}(?![\w-])")),
    ("flag", re.compile(r"(?<![\w-])--[a-z][\w-]*")),
    ("envvar", re.compile(r"(?<![\w])[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+(?![\w])")),
    ("job", re.compile(r"\b(?:job|workflow)s?\s+[`'\"][^`'\"\n]+[`'\"]", re.I)),
    ("tool", re.compile(r"(?<![\w-])(?:cargo|pytest|tmux|clippy|rustc|npm|pnpm|launchctl|ripgrep|git|gh)(?![\w-])")),
    ("identifier", re.compile(r"`[^`\n]+`")),
)


def lint(text, level=None):
    """Warnings for `text` at `level` (unknown or None: pm). [] at engineer and above. Allowed: URLs and #N refs."""
    if tl.normalize(level) not in LINTED:
        return []
    text = str(text or "")
    masked = _URL.sub(lambda m: " " * len(m.group(0)), text)   # keep offsets so line numbers stay right
    found, taken = [], []
    for rule, rx in _RULES:
        for m in rx.finditer(masked):
            s, e = m.span()
            if any(s < te and ts < e for ts, te in taken):
                continue   # an earlier, more specific rule already flagged this span
            taken.append((s, e))
            found.append({"rule": rule, "match": m.group(0), "line": text.count("\n", 0, s) + 1, "_s": s})
    found.sort(key=lambda w: w["_s"])
    for w in found:
        del w["_s"]
    return found


def format_warnings(warnings, level, label=""):
    """One stderr/stdout line per warning, naming the level so the author knows which rule applied."""
    where = f"{label}: " if label else ""
    return [f"level_lint [{tl.normalize(level)}]: {where}{w['rule']} `{w['match']}` (line {w['line']})" for w in warnings]


def item_texts(data):
    """(field, text) pairs of an item write: title, detail, and each option's text (a string option, or label and desc)."""
    texts = [(f, data.get(f)) for f in ("title", "detail")]
    opts = data.get("options")
    for i, o in enumerate(opts if isinstance(opts, list) else []):
        if isinstance(o, str):
            texts.append((f"options[{i}]", o))
        elif isinstance(o, dict):
            texts += [(f"options[{i}].{k}", o.get(k)) for k in ("label", "desc")]
    return [(f, t) for f, t in texts if isinstance(t, str) and t]


def item_level(store, doc_id, data):
    """The level an item write is linted at: its valid `level_override` (in the write, else stored), else the owner's
    setting read through `store`; pm when neither is readable."""
    override = data.get("level_override")
    if override is None:
        try:
            override = (store.get("items", doc_id)["data"] or {}).get("level_override")
        except Exception:  # noqa: BLE001 - a new id, or an unreadable doc: fall through to the setting
            override = None
    if override in tl.LEVELS:
        return override
    try:
        settings = store.get("meta", "settings")["data"]
    except Exception:  # noqa: BLE001 - no settings doc reads as the default
        return tl.DEFAULT
    return tl.normalize(settings.get("technical_level") if isinstance(settings, dict) else None)


def warn_item(store, doc_id, data, out=None):
    """Print one stderr warning per lint finding in an item write; never refuse, never raise (#11064: warn only).
    Returns the number of warnings printed (0 when the lint itself failed; that failure is named on stderr)."""
    out = out or sys.stderr
    try:
        if not isinstance(data, dict):
            return 0
        level = item_level(store, doc_id, data)
        lines = [ln for field, text in item_texts(data)
                 for ln in format_warnings(lint(text, level), level, f"item {doc_id} {field}")]
        for ln in lines:
            print(f"warning: {ln}", file=out)
        return len(lines)
    except Exception as e:  # noqa: BLE001 - a lint problem must never refuse or lose a write
        print(f"warning: level lint skipped: {e!r}", file=out)
        return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description="Warn on detail the technical level does not allow.")
    ap.add_argument("--level", help="one of " + ", ".join(tl.LEVELS) + "; default: the stored setting")
    ap.add_argument("--strict", action="store_true", help="exit 1 when there is any warning")
    ap.add_argument("--file", help="read the text from this file (else the TEXT arguments, else stdin)")
    ap.add_argument("text", nargs="*")
    args = ap.parse_args(argv)
    if args.file:
        with open(args.file, encoding="utf-8") as f:
            text = f.read()
    elif args.text:
        text = " ".join(args.text)
    else:
        text = sys.stdin.read()
    level = args.level if args.level is not None else tl.read()
    if level not in tl.LEVELS:
        print(f"level_lint: unknown level {level!r}; using {tl.DEFAULT}", file=sys.stderr)
    out = format_warnings(lint(text, level), level)
    for ln in out:
        print(ln)
    return 1 if (out and args.strict) else 0


if __name__ == "__main__":
    sys.exit(main())
