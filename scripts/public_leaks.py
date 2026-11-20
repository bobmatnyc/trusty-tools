"""public_leaks.py — the `check_content.py public-leaks` mode: a whole-tree private-content gate.

Why: the repository is public, and private content (rig facts, internal notes,
  personal processes, employer material) entered it with nothing to stop it.
  The Architect leak gate covered one subtree; this mode reuses its loader and
  matcher (load_leak_rules, leak_findings) over every tracked text file.
What: scans `git ls-files` (or the paths given, for the pre-commit hook) with
  scripts/public_leak_patterns.txt plus the PRIVATE_NAME class: names read
  from the out-of-tree file named by PUBLIC_LEAK_PRIVATE_NAMES_FILE. In CI that
  file is written from the PUBLIC_LEAK_PRIVATE_NAMES Actions secret, and an
  absent or empty file fails the run; locally it is skipped with a notice.
  Hits of the pattern classes are counted per (path, pattern id) and compared
  with scripts/public_leak_baseline.tsv, which can only shrink: a count above
  the baseline fails, a count below it passes with a note, and an entry whose
  hits are all gone fails with "delete the entry". PRIVATE_NAME is never
  baselined: every hit fails unless its (path, matched run) pair is listed,
  with a reason, in scripts/public_leak_private_allowlist.tsv. No matched text
  is printed.
  Regenerate the baseline: python3 scripts/check_content.py public-leaks --update-baseline
Test: scripts/check_public_leaks_selftest.sh
"""

import hashlib
import os
import re
import subprocess
from collections import Counter
from pathlib import Path

PATTERNS_REL = "scripts/public_leak_patterns.txt"
BASELINE_REL = "scripts/public_leak_baseline.tsv"
ALLOW_REL = "scripts/public_leak_private_allowlist.tsv"
# These name the patterns or paths they hunt for; every other file is scanned.
SELF_FILES = {PATTERNS_REL, BASELINE_REL, ALLOW_REL}
PRIVATE = "PRIVATE_NAME"
NAMES_ENV = "PUBLIC_LEAK_PRIVATE_NAMES_FILE"
RUN_RE = re.compile(r"[A-Za-z0-9]+")
BASELINE_HEADER = (
    "# Public-content leak baseline: path<TAB>pattern-id<TAB>hit-count. Shrink-only.\n"
    "# Regenerate: python3 scripts/check_content.py public-leaks --update-baseline\n"
)


class _Hit:
    """The one Match method leak_findings reads."""

    def __init__(self, text):
        self.text = text

    def group(self, _):
        return self.text


class PrivateNames:
    """Private names matched as substrings of each alphanumeric run.

    Why: a name glued to other text (CamelCase, a suffix, digits, a plural)
      escapes a whole-word match, and the names cannot be committed (#11513).
    What: duck-types a compiled regex for leak_findings. Each name is lowercased
      to [a-z0-9]; each [A-Za-z0-9]+ run of the line is lowercased and every
      name found inside it is a hit. A substring search over the whole run
      covers every case-change and letter/digit split of it.
    Test: selftest cases private-camel, private-glued, private-digits,
      private-plural, private-hyphen, private-underscore, private-url, private-at.
    """

    def __init__(self, names):
        self.names = names

    def finditer(self, line):
        low = line.lower()
        if not any(n in low for n in self.names):
            return
        for run in RUN_RE.finditer(low):
            for name in self.names:
                if name in run.group(0):
                    yield _Hit(run.group(0))

    def search(self, line):
        return next(self.finditer(line), None)


def _private_names(fail):
    """Names from the out-of-tree file; fail closed in CI, skip with a notice locally."""
    path = os.environ.get(NAMES_ENV, "")
    in_ci = os.environ.get("CI") == "true" or os.environ.get("GITHUB_ACTIONS") == "true"
    names = []
    if path and Path(path).is_file():
        for line in Path(path).read_text(encoding="utf-8").splitlines():
            name = re.sub(r"[^a-z0-9]", "", line.strip().lower())
            if line.strip().startswith("#") or not name:
                continue
            if len(name) < 4:
                raise fail(f"PATTERNS: {NAMES_ENV}: a name needs 4+ letters or digits")
            names.append(name)
    if names:
        return PrivateNames(sorted(set(names)))
    if in_ci:
        raise fail(f"PATTERNS: {NAMES_ENV} is absent or empty in CI; set the "
                   "PUBLIC_LEAK_PRIVATE_NAMES Actions secret (see scripts/public_leaks.py)")
    print(f"check_content public-leaks: {NAMES_ENV} absent or empty, private names skipped")
    return None


def _rules(load_leak_rules, root, fail):
    """In-tree pattern rules plus the out-of-tree PRIVATE_NAME rule, when present."""
    patterns = Path(os.environ.get("PUBLIC_LEAK_PATTERNS") or root / PATTERNS_REL)
    rules = load_leak_rules(patterns, deny="")
    private = _private_names(fail)
    return rules + [(PRIVATE, private, None)] if private else rules


def _tracked(root, paths):
    if paths:
        rels = [Path(p).resolve().relative_to(root.resolve()).as_posix() for p in paths]
    else:
        out = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], check=True, capture_output=True).stdout
        rels = [r for r in out.decode("utf-8", "surrogateescape").split("\0") if r]
    for rel in sorted(set(rels) - SELF_FILES):
        p = root / rel
        if p.is_file() and not p.is_symlink():
            data = p.read_bytes()
            if b"\0" not in data[:8192]:  # text files only
                yield rel, data


def read_baseline(path, fail):
    """`{(path, id): count}` from the TSV; a malformed row fails closed."""
    base = {}
    if not path.is_file():
        return base
    for num, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        f = line.split("\t")
        if len(f) != 3 or not f[2].isdigit() or int(f[2]) < 1:
            raise fail(f"BASELINE: {BASELINE_REL}:{num}: expected path<TAB>pattern-id<TAB>count>=1")
        if f[1] == PRIVATE:
            raise fail(f"BASELINE: {BASELINE_REL}:{num}: PRIVATE_NAME is never baselined; "
                       f"remove the name, or list the path with a reason in {ALLOW_REL}")
        base[(f[0], f[1])] = int(f[2])
    return base


def _allow_keys(rel):
    """The allowlist forms that name `rel`: the plain path and `sha256:<hex of the path>`.

    Why: a path that itself spells a private name cannot sit in the allowlist in
      clear text, and the allowlist is exempt from scanning (#11513).
    What: a row may give `sha256:` plus the hex SHA-256 of the UTF-8 path; it
      matches that one file only, never a directory tree.
    Test: selftest cases private-allow-digest, private-allow-digest-stale.
    """
    return {rel, "sha256:" + hashlib.sha256(rel.encode("utf-8")).hexdigest()}


TOKEN_RE = re.compile(r"[a-z0-9]+")


def read_allowlist(path, fail):
    """`{(path-key, token): False}` of PRIVATE_NAME hits accepted, one row per alias.

    Why: the owner kept the deprecated org-named aliases, removed in the next
      major release (#11535); a whole-file exemption would also pass any other
      private name in that file, so each row names the one run it accepts.
    What: rows are path<TAB>token<TAB>reason naming an issue (#N). path is the
      plain path or `sha256:<hex of the path>`; token is the lowercased
      alphanumeric run the hit matched (e.g. the env-var segment). The value
      flips to True once a hit uses the row; an unused row is stale.
    Test: selftest cases private-allowlisted, private-allow-other-token,
      private-allow-other-path, private-allow-stale, private-allow-reason.
    """
    allowed = {}
    if not path.is_file():
        return allowed
    for num, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        f = line.split("\t")
        if len(f) != 3 or not TOKEN_RE.fullmatch(f[1]) or not re.search(r"#[0-9]+", f[2]):
            raise fail(f"ALLOWLIST: {ALLOW_REL}:{num}: expected path<TAB>token<TAB>reason naming an issue (#N)")
        allowed[(f[0], f[1])] = False
    return allowed


def _excused(allow, cls, rel, token):
    """True when a PRIVATE_NAME hit's (path, token) pair is allowlisted; marks the row used."""
    if cls != PRIVATE:
        return False
    for key in _allow_keys(rel):
        if (key, token) in allow:
            allow[(key, token)] = True
            return True
    return False


def run(root, load_leak_rules, leak_findings, fail, paths=(), update=False):
    """Scan, then compare with (or rewrite) the shrink-only baseline."""
    rules = _rules(load_leak_rules, root, fail)
    have_private = any(cls == PRIVATE for cls, _, _ in rules)
    files = list(_tracked(root, paths))
    if not files and not paths:
        raise fail("SCAN_ROOT: git ls-files returned no text files")
    allow = read_allowlist(root / ALLOW_REL, fail)
    hits, lines = Counter(), {}
    for cls, rel, num, shown in leak_findings(files, rules):
        if _excused(allow, cls, rel, shown):
            continue
        hits[(rel, cls)] += 1
        lines.setdefault((rel, cls), num)
    for rel, _ in files:  # a path that itself names a private place is a hit too
        for cls, rx, ok in rules:
            bad = [m.group(0) for m in rx.finditer(rel) if ok is None or not ok.fullmatch(m.group(0))]
            if any(not _excused(allow, cls, rel, tok) for tok in bad):
                hits[(rel, cls)] += 1
                lines.setdefault((rel, cls), 0)
    errors = []
    if have_private:
        scanned_keys = {k for r, _ in files for k in _allow_keys(r)}
        for (key, token), used in sorted(allow.items()):
            if not used and (not paths or key in scanned_keys):
                errors.append(f"STALE_ALLOWLIST: {key}: a row matches no PRIVATE_NAME hit — delete the row from {ALLOW_REL}")
    baseline_path = root / BASELINE_REL
    if update:
        if paths:
            raise fail("USAGE: --update-baseline scans the whole tree; pass no paths")
        rows = sorted(f"{rel}\t{cls}\t{n}\n" for (rel, cls), n in hits.items() if cls != PRIVATE)
        baseline_path.write_text(BASELINE_HEADER + "".join(rows), encoding="utf-8")
        print(f"check_content public-leaks: baseline rewritten, {len(rows)} entr(y/ies)")
        return
    base = read_baseline(baseline_path, fail)
    scanned = {rel for rel, _ in files}
    notes = 0
    for key, n in sorted(hits.items()):
        allowed = base.get(key, 0)
        if n > allowed:
            errors.append(f"{key[1]}: {key[0]}:{lines[key]}: {n} hit(s), baseline allows {allowed}")
        elif n < allowed:
            notes += 1
    for key, n in sorted(base.items()):
        if key not in hits and (not paths or key[0] in scanned):
            errors.append(f"STALE_BASELINE: {key[0]} {key[1]}: no hit left — delete the entry from {BASELINE_REL}")
    if errors:
        errors.append("Remove the private content, or move it out of tree; see the header of " + PATTERNS_REL)
        raise fail("\n".join(errors))
    shrink = f"; {notes} baseline count(s) can shrink (run --update-baseline)" if notes else ""
    print(f"check_content public-leaks: {len(files)} file(s) scanned, {sum(hits.values())} baselined hit(s) — OK{shrink}")

