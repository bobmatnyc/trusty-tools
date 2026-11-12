#!/usr/bin/env python3
"""technical_level.py: the settable technical level for owner-facing text (#11064, spec TL-1, TL-4..TL-8).

Why: the owner asked for owner-facing text (chat, dashboard items, briefs) at a settable altitude instead of one
fixed engineer depth. Each user reads at their own level; relays and records stay technical and ignore it.
What: LEVELS is the ordered six-value tuple and RULES the one-line rule per level (the only table). read(root) is
the owner's level from meta/settings.technical_level; read_user(name) a secondary user's, from
local/users/registry.json or the profile's "Technical level:" line, never the owner's; for_item(item, root) honours
an item's `level_override` and leaves the setting alone. Every reader returns DEFAULT ("pm") for an unset or unknown
value. refusal(coll, doc_id, data) is the text dashboard_store refuses a write with when `technical_level` (meta/settings)
or `level_override` (items) is not one of LEVELS. speaker(prompt) names the secondary user of a marked prompt.
line(level, who) is the hook text "TECH LEVEL: <level> (<rule>)". CLI: technical_level.py [--user NAME] prints a level.
Test: tests/test_technical_level.py
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)

LEVELS = ("non-technical", "vibe-coder", "pm", "engineer", "senior-engineer", "cto")
DEFAULT = "pm"
USERS_DIR = os.path.join(os.path.dirname(HERE), "local", "users")
# Keys a dashboard write may carry a level in, per collection (#11064 rulings a and TL-6).
LEVEL_KEYS = {("meta", "settings"): "technical_level", ("items", None): "level_override"}

# One line per level, from spec section 2. The hook prints it; keep each under ~110 characters.
RULES = {
    "non-technical": "everyday words, business impact, 2-3 sentences, no paths, SHAs, flags or tool names",
    "vibe-coder": "product words (app, deploy, repo), what the user sees, 3-4 sentences, no line numbers, SHAs or flags",
    "pm": "outcome, cost, risk, reversibility, 4-6 sentences; issue and PR links ok, no file paths, SHAs, flags or job names",
    "engineer": "full technical terms, module names, root cause and trade-offs, 6-10 sentences with a short evidence list",
    "senior-engineer": "design trade-offs, failure modes, rollout and rollback, file:line evidence, blast radius",
    "cto": "risk to production and customers, cost, reversibility, the contract at stake; line detail stays in the brief",
}

SPEAKER = re.compile(r"^\[SECONDARY USER ([A-Za-z0-9._-]+) via ")
PROFILE_LINE = re.compile(r"^\s*[-*]?\s*Technical level:\s*`?([a-z-]+)`?\s*$", re.I | re.M)
USER_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")


def normalize(value):
    """The level for `value`: itself when one of LEVELS, else DEFAULT (unset or unknown fails to pm, never higher)."""
    return value if isinstance(value, str) and value in LEVELS else DEFAULT


def read(root=None):
    """The owner's level from meta/settings through dashboard_store; DEFAULT when the doc or key is missing or invalid."""
    import dashboard_store as ds   # lazy: dashboard_store imports this module for refusal()
    try:
        data = ds.Store(root or ds.DEFAULT_ROOT).get("meta", "settings")["data"]
    except ds.NotFound:
        return DEFAULT
    return normalize(data.get("technical_level") if isinstance(data, dict) else None)


def for_item(item, root=None):
    """The level an item's text is written at: its valid `level_override` (a "more detail" request, TL-9), else the
    owner's setting. Reads only; the setting is never changed by an item."""
    override = item.get("level_override") if isinstance(item, dict) else None
    return override if override in LEVELS else read(root)


def _registry_level(name, users_dir):
    """The registry entry's `technical_level` for `name`, or None (no registry, no entry, no field, bad JSON)."""
    try:
        with open(os.path.join(users_dir, "registry.json"), encoding="utf-8") as f:
            users = json.load(f).get("users")
    except (OSError, ValueError, AttributeError):
        return None
    for u in users if isinstance(users, list) else []:
        if isinstance(u, dict) and u.get("name") == name and "technical_level" in u:
            return u["technical_level"]
    return None


def read_user(name, users_dir=None):
    """A secondary user's level (TL-5, TL-8): registry.json entry first, then `local/users/<name>.md`'s
    "Technical level:" line; DEFAULT when neither holds a valid level. The owner's setting is never consulted."""
    if not isinstance(name, str) or not USER_NAME.match(name):
        return DEFAULT
    users_dir = users_dir or USERS_DIR
    level = _registry_level(name, users_dir)
    if level is None:
        try:
            with open(os.path.join(users_dir, name + ".md"), encoding="utf-8") as f:
                m = PROFILE_LINE.search(f.read())
            level = m.group(1).lower() if m else None
        except (OSError, ValueError):
            level = None
    return normalize(level)


def speaker(prompt):
    """The secondary user named by a `[SECONDARY USER <name> via <channel> ...]` prompt, else None (the owner)."""
    m = SPEAKER.match(str(prompt or "").lstrip())
    return m.group(1) if m else None


def refusal(coll, doc_id, data):
    """The refusal text when `data` (the written fields) sets a level key of (coll, doc_id) to a value outside LEVELS,
    else None. Only a key present in the write is checked, so an unrelated write never fails on it."""
    key = LEVEL_KEYS.get((coll, doc_id)) or LEVEL_KEYS.get((coll, None))
    if not key or not isinstance(data, dict) or key not in data or data[key] in LEVELS:
        return None
    return (f"{coll}/{doc_id} write refused: {key} {json.dumps(data[key], ensure_ascii=False)[:40]} is not a "
            f"technical level (expected one of: {', '.join(LEVELS)})")


def line(level, who=None):
    """The hook line for `level`: "TECH LEVEL: <level> (<rule>)"; `who` names a secondary user it applies to."""
    level = normalize(level)
    rule = RULES[level] + (f"; secondary user {who}, not the owner's level" if who else "")
    return f"TECH LEVEL: {level} ({rule})"


def main(argv=None):
    """Print the owner's level, or with --user NAME a secondary user's; always exit 0 (the reader never fails)."""
    import argparse
    ap = argparse.ArgumentParser(description="Print the technical level owner-facing text is written at.")
    ap.add_argument("--user", help="a secondary user's name (local/users/); omit for the owner")
    ap.add_argument("--root", help="dashboard store root (default: local/owner-queue)")
    args = ap.parse_args(argv)
    print(read_user(args.user) if args.user else read(args.root))
    return 0


if __name__ == "__main__":
    sys.exit(main())
