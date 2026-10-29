"""content_probe_lint.py — the probe-then-degrade rule for content assets (#8390).

Why: ADR-0064 decision 6 — any agent or skill must work with any harness. An
  asset that runs a harness command (`tm`, `tcode`, `tagent`) unconditionally
  breaks under a harness that lacks it. The asset must check the command is
  there first and say what to do when it is not.

What: `lint(content_dir)` returns one error per shared asset whose first
  command of a harness comes before any probe-then-degrade for that harness.
  - A command is any fenced code line naming the harness binary followed by a
    subcommand or flag, wherever it sits on the line (after `&&`, `;`, `$(`),
    or any inline code span `<bin> <sub>…`, including one wrapped over lines
    (matched on the paragraph joined with spaces, #10104). A span is a prohibition, not a
    command, only when a negation governs it directly: "never", "not", "do
    not" or "n't", optionally followed by one verb, right before the span in
    its own sentence (sentences split on `.`, `!`, `?` and `;`).
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
# #8390 critic round: anywhere on a fenced line, not only at its start.
FENCED_CMD_RE = re.compile(rf"(?<![\w./-])({BIN}) +[a-z-][^;&|)#`]*")
INLINE_CMD_RE = re.compile(rf"`(({BIN}) [a-z][^`]*)`")
# A negation that governs the span: it ends the sentence prefix, at most one
# verb between it and the span.
GOVERNING_NEGATION_RE = re.compile(
    r"(?i)(?:\b(?:never|not|cannot)|n't)\s+(?:[a-z]+\s+)?$"
)
SENTENCE_BREAK_RE = re.compile(r"[.!?;]\s|;")
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


def inline_commands(para):
    """Yield `(line_no, binary, text)` for each inline span in `para`, a list of
    `(line_no, line)` joined with spaces so a span wrapped over lines still
    matches (#10104). A span reports the line of its opening backtick."""
    text = " ".join(line for _, line in para)
    starts, pos = [], 0
    for num, line in para:
        starts.append((pos, num))
        pos += len(line) + 1
    for m in INLINE_CMD_RE.finditer(text):
        sentence = SENTENCE_BREAK_RE.split(text[: m.start()])[-1]
        if not GOVERNING_NEGATION_RE.search(sentence):
            num = [n for start, n in starts if start <= m.start()][-1]
            yield num, m.group(2), " ".join(m.group(1).split())


def commands(lines):
    """Yield `(line_no, binary, text)` for each harness command in `lines`."""
    in_fence = False
    para = []  # (line_no, line) of the open non-fenced paragraph
    for num, line in enumerate(lines, 1):
        fence = FENCE_RE.match(line)
        if fence or (not in_fence and not line.strip()):
            yield from inline_commands(para)
            para = []
            if fence:
                in_fence = not in_fence
            continue
        if in_fence:
            m = FENCED_CMD_RE.search(line)
            if m:
                yield num, m.group(1), m.group(0).strip()
            continue
        para.append((num, line))
    yield from inline_commands(para)


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
