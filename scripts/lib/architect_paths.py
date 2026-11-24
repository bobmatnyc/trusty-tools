#!/usr/bin/env python3
"""architect_paths.py — the parser behind scripts/check_architect_paths.sh (#9997 AC1).

Why: a fresh Architect reads its operating manual and its skills, and every
path and skill they name must exist after `tm fleet init`, or the Architect
acts on a file or a procedure that is not there. A check that reads only
backticked words misses a path inside a fenced block and a script named in a
command (`python3 scripts/x.py`), so all three forms are parsed.
What: reads the manual (`<root>/content/instructions/architect/architect.md`),
the seeded `CLAUDE.md`, every `<init-dir>/.claude/skills/*/SKILL.md` and, since
#10161 D3c, every `<init-dir>/.claude/agents/*.md`.
From each it takes path tokens from inline code spans (`backtick`), fenced
code blocks (`fence`) and `python3|python|bash|sh <path>` commands anywhere
(`command`). A token counts when it starts with one of PREFIXES or is
`CLAUDE.md`; a `<placeholder>` or `*` cuts it to its directory. Each must
exist under the init dir (FAIL PATH). Every skill named in a
`(details: ...)` pointer, a skill-index row or a backticked `architect-*`
word must have a `SKILL.md` (FAIL SKILL). A text that cannot be read or
decoded is FAIL READ; no skills, or no agents, is FAIL EMPTY. It never skips.
Test: scripts/check_architect_paths_selftest.sh plants one fault per form.
Usage: architect_paths.py --root <repo> --init-dir <dir>
  Exit 0: every reference resolves. Exit 1: a FAIL line per finding.
  Exit 2: usage error.
"""
import argparse
import os
import re
import sys

PREFIXES = ("scripts/", "local/", ".claude/", "docs/", "templates/", "owner/")
MANUAL = os.path.join("content", "instructions", "architect", "architect.md")
FENCE = re.compile(r"^\s*(```|~~~)")
INLINE = re.compile(r"`([^`\n]+)`")
COMMAND = re.compile(r"(?:^|[\s(`'\"])(?:python3?|bash|sh)\s+(\S+)")
DETAILS = re.compile(r"\(details:\s*([^)]*)\)")
SKILL_NAME = re.compile(r"\b((?:architect|tm)-[a-z0-9]+(?:-[a-z0-9]+)*)\b")
INDEX_ROW = re.compile(r"^\|\s*`((?:architect|tm)-[a-z0-9-]+)`\s*\|")
TICK_SKILL = re.compile(r"^architect-[a-z0-9]+(?:-[a-z0-9]+)*$")
SPLIT = re.compile(r"[\s`'\"(),;=]+")


def path_tokens(text):
    """Every token in `text` that names a path under the Architect directory."""
    out = []
    for raw in SPLIT.split(text):
        word = raw.rstrip(".:;!?")
        if word == "CLAUDE.md" or word.startswith(PREFIXES):
            out.append(word)
    return out


def references(text):
    """(line_no, form, path) for each path reference in a Markdown text."""
    refs = []
    in_fence = False
    for n, line in enumerate(text.splitlines(), 1):
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            refs += [(n, "fence", p) for p in path_tokens(line)]
            continue
        for span in INLINE.findall(line):
            refs += [(n, "backtick", p) for p in path_tokens(span)]
        for target in COMMAND.findall(line):
            refs += [(n, "command", p) for p in path_tokens(target)]
    return refs


def skill_names(text):
    """(line_no, skill) for each skill a text names as a skill."""
    names = []
    in_fence = False
    for n, line in enumerate(text.splitlines(), 1):
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for body in DETAILS.findall(line):
            names += [(n, s) for s in SKILL_NAME.findall(body)]
        row = INDEX_ROW.match(line)
        if row:
            names.append((n, row.group(1)))
        names += [(n, s) for s in INLINE.findall(line) if TICK_SKILL.match(s)]
    return names


def resolve(init_dir, path):
    """The filesystem path a reference must exist at."""
    cut = min((i for i in (path.find("<"), path.find("*")) if i >= 0), default=-1)
    if cut >= 0:
        path = path[:cut]
        path = path[: path.rfind("/") + 1] if "/" in path else ""
    return os.path.join(init_dir, path)


def read(path, fails, label):
    try:
        with open(path, "rb") as f:
            return f.read().decode("utf-8")
    except (OSError, UnicodeDecodeError) as e:
        fails.append(f"FAIL READ: {label}: cannot read {path}: {type(e).__name__}: {e}")
        return None


def check(root, init_dir):
    """Return (fail_lines, paths_checked, skills_checked)."""
    fails = []
    texts = []
    skills_dir = os.path.join(init_dir, ".claude", "skills")
    try:
        stems = sorted(e for e in os.listdir(skills_dir)
                       if os.path.isdir(os.path.join(skills_dir, e)))
    except OSError as e:
        fails.append(f"FAIL READ: .claude/skills: {type(e).__name__}: {e}")
        stems = []
    if not stems and not fails:
        fails.append(f"FAIL EMPTY: .claude/skills holds no skill under {init_dir}")
    # #10161 D3c: the Architect agents name scripts, records and skills too.
    agents_dir = os.path.join(init_dir, ".claude", "agents")
    try:
        agents = sorted(e for e in os.listdir(agents_dir) if e.endswith(".md"))
    except OSError as e:
        fails.append(f"FAIL READ: .claude/agents: {type(e).__name__}: {e}")
        agents = None
    if agents == []:
        fails.append(f"FAIL EMPTY: .claude/agents holds no agent under {init_dir}")
    for label, path in ([("architect.md", os.path.join(root, MANUAL)),
                         ("CLAUDE.md", os.path.join(init_dir, "CLAUDE.md"))]
                        + [(f".claude/skills/{s}/SKILL.md",
                            os.path.join(skills_dir, s, "SKILL.md")) for s in stems]
                        + [(f".claude/agents/{a}", os.path.join(agents_dir, a))
                           for a in agents or []]):
        text = read(path, fails, label)
        if text is not None:
            texts.append((label, text))
    paths = skills = 0
    for label, text in texts:
        for n, form, ref in references(text):
            paths += 1
            if not os.path.exists(resolve(init_dir, ref)):
                fails.append(f"FAIL PATH: {label}:{n}: `{ref}` ({form}) is missing "
                             f"after tm fleet init")
        for n, name in skill_names(text):
            skills += 1
            if not os.path.isfile(os.path.join(skills_dir, name, "SKILL.md")):
                fails.append(f"FAIL SKILL: {label}:{n}: skill `{name}` has no "
                             f".claude/skills/{name}/SKILL.md after tm fleet init")
    if texts and not paths:
        fails.append("FAIL EMPTY: no path reference found in any text; the parser "
                     "or the input is wrong")
    return fails, paths, skills


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", required=True)
    ap.add_argument("--init-dir", required=True)
    args = ap.parse_args(argv)
    fails, paths, skills = check(args.root, args.init_dir)
    for line in fails:
        print(line)
    if fails:
        print(f"check_architect_paths: FAILED ({len(fails)} findings)", file=sys.stderr)
        return 1
    print(f"check_architect_paths: OK ({paths} path references, {skills} skill "
          f"references resolve)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
