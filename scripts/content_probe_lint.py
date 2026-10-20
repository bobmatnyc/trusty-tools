"""content_probe_lint.py — the probe-then-degrade rule for content assets (#8390).

Why: ADR-0064 decision 6 — any agent or skill must work with any harness. An
  asset that runs a harness command (`tm`, `tcode`, `tagent`) unconditionally
  breaks under a harness that lacks it. The asset must check the command is
  there first and say what to do when it is not.

What: `lint(content_dir)` returns one error per shared asset whose first
  command of a harness comes before any probe-then-degrade for that harness.
  - A command is a fenced code line that starts with the harness binary
    (after an optional `$ ` prompt and `VAR=value` prefixes), or an inline
    code span `<bin> <sub>…` right after "run", "use", "call", "invoke",
    "execute", "try" or "poll with". A span in a sentence that says "never",
    "not" or "n't" before it is a prohibition, not a command.
  - A probe is a line naming `command -v <bin>`, `<bin> --version` or
    `<bin> --help` in a paragraph (or fenced block) that also says what to do
    without it: "absent", "missing", "unavailable", "not installed",
    "not available", "not found", "fall back"/"fallback", "otherwise", or a
    shell `||`. It covers a command in its own paragraph or any later one.
  - A harness's own assets run its commands freely: tm owns the `tm` and
    `tm-*` skills and `instructions/` (minus the trees below); tcode owns
    `agents/trusty-code/` and `instructions/trusty-code/`; tagent owns
    `agents/trusty-agents/`. `instructions/harness_understanding/` and
    `instructions/trusty-review/` are owned by no harness.

Test: scripts/check_content_selftest.sh, the probe-* cases.
"""

import re
from pathlib import Path

CLASSES = ("instructions", "agents", "skills")
HARNESSES = ("tm", "tcode", "tagent")
# Owned prefixes (content/-relative); a path is owned when it starts with one.
OWNED = {
    "tm": ("skills/tm.md", "skills/tm/", "skills/tm-", "instructions/"),
    "tcode": ("agents/trusty-code/", "instructions/trusty-code/"),
    "tagent": ("agents/trusty-agents/",),
}
# Under tm's `instructions/` but read by other harnesses too.
NOT_TM = ("instructions/trusty-code/", "instructions/trusty-review/", "instructions/harness_understanding/")

BIN = "|".join(HARNESSES)
FENCE_RE = re.compile(r"^\s*(```|~~~)")
FENCED_CMD_RE = re.compile(rf"^\s*(?:\$\s+)?(?:[A-Z_][A-Z0-9_]*=\S+\s+)*({BIN})(?=\s+\S)")
INLINE_CMD_RE = re.compile(rf"(?i:\b(?:run|use|call|invoke|execute|try|poll with)\s+)`(({BIN}) [a-z][^`]*)`")
NEGATION_RE = re.compile(r"(?i)\b(never|not|don't|do not|doesn't|cannot)\b|n't\b")
DEGRADE_RE = re.compile(r"(?i)absent|missing|unavailable|not installed|not available|not found|fall(s)? ?back|otherwise|\|\|")


def probe_re(binary):
    return re.compile(rf"command -v {binary}\b|\b{binary} --(version|help)\b")


def owner_of(rel):
    """The harness that owns `rel`, or None for a shared asset."""
    for binary, prefixes in OWNED.items():
        if binary == "tm" and rel.startswith(NOT_TM):
            continue
        if rel.startswith(prefixes):
            return binary
    return None


def commands(lines):
    """Yield `(line_no, binary, text)` for each harness command in `lines`."""
    in_fence = False
    for num, line in enumerate(lines, 1):
        if FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            m = FENCED_CMD_RE.match(line)
            if m:
                yield num, m.group(1), line.split("#", 1)[0].strip().lstrip("$ ").strip()
            continue
        for m in INLINE_CMD_RE.finditer(line):
            sentence = re.split(r"[.!?]\s", line[: m.start()])[-1]
            if not NEGATION_RE.search(sentence):
                yield num, m.group(2), m.group(1)


def blocks(lines):
    """Yield `(first_line_no, [lines])` per paragraph or fenced block."""
    block, start, in_fence = [], 0, False
    for num, line in enumerate(lines, 1):
        fence = FENCE_RE.match(line)
        if fence or (not in_fence and not line.strip()):
            if block:
                yield start, block
            block, start = [], 0
            if fence:
                in_fence = not in_fence
            continue
        if not block:
            start = num
        block.append(line)
    if block:
        yield start, block


def first_probe(lines, binary):
    """The first line of the first block holding a probe-then-degrade for
    `binary`, or None. A probe in the same paragraph as a command covers it."""
    probe = probe_re(binary)
    for start, block in blocks(lines):
        text = "\n".join(block)
        if not DEGRADE_RE.search(text):
            continue
        if any(probe.search(line) for line in block):
            return start
    return None


def lint_text(rel, text):
    """Errors for one asset, `rel` relative to content/."""
    owner = owner_of(rel)
    lines = text.splitlines()
    errors, seen = [], set()
    for num, binary, cmd in commands(lines):
        if binary == owner or binary in seen:
            continue
        seen.add(binary)
        probed = first_probe(lines, binary)
        if probed is None or probed > num:
            errors.append(
                f"content/{rel}:{num}: runs `{cmd}` with no probe-then-degrade before it; "
                f"add a line such as \"Check `command -v {binary}` first; if `{binary}` is absent, <what to do instead>.\" "
                "(ADR-0064 decision 6)"
            )
    return errors


def lint(content):
    """Every probe-then-degrade error under `content` (a content/ directory)."""
    errors = []
    for cls in CLASSES:
        base = content / cls
        if not base.is_dir() or base.is_symlink():
            continue
        for path in sorted(base.rglob("*.md")):
            if path.is_symlink() or not path.is_file():
                continue
            rel = path.relative_to(content).as_posix()
            try:
                text = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue  # scan_tree reports an undecodable file.
            errors.extend(lint_text(rel, text))
    return errors
