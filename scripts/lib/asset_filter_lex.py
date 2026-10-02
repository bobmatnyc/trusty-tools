"""asset_filter_lex.py — the Rust lexing helpers of check_asset_test_filter.py.

Why: split out of scripts/check_asset_test_filter.py to keep that file inside
  its line-cap budget when #9011 added rule R4. Pure functions, no I/O.
What: `lex` (blank comments and literal bodies, keep newlines), `match_close`,
  `attrs_before` and `expand_use`.
Test: scripts/check_asset_test_filter_selftest.sh, through the guard.
"""
import re

IDENT = r"[A-Za-z_][A-Za-z0-9_]*"
RAW_START = re.compile(r'(?:b|c)?r(#*)"')


# ------------------------------------------------------------------- lexing
def is_ident_char(ch):
    return ch.isalnum() or ch == "_"


def lex(src):
    """Blank comments and literal bodies (newlines kept); return masked text and literals."""
    out = list(src)
    lits = []
    n = len(src)
    i = 0

    def blank(a, b):
        for k in range(a, min(b, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
            continue
        if src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
            continue
        if c in "bcr" and (i == 0 or not is_ident_char(src[i - 1])):
            m = RAW_START.match(src, i)
            if m:
                close = '"' + m.group(1)
                j = src.find(close, m.end())
                j = n if j < 0 else j
                lits.append((i, src[m.end():j]))
                blank(m.end(), j)
                i = j + len(close)
                continue
            if c in "bc" and src.startswith('"', i + 1):
                i += 1
                c = '"'
        if c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            lits.append((i, src[i + 1:j]))
            blank(i + 1, j)
            i = j + 1
            continue
        if c == "'":
            if src.startswith("\\", i + 1):
                j = src.find("'", i + 3)
                j = n if j < 0 else j
                blank(i + 1, j)
                i = j + 1
                continue
            if i + 2 < n and src[i + 2] == "'":
                blank(i + 1, i + 2)
                i += 3
                continue
        i += 1
    return "".join(out), lits


def match_close(masked, open_at):
    """Offset just past the bracket closing the one at `open_at`."""
    pairs = {"{": "}", "(": ")", "[": "]"}
    stack = []
    for k in range(open_at, len(masked)):
        ch = masked[k]
        if ch in pairs:
            stack.append(pairs[ch])
        elif stack and ch == stack[-1]:
            stack.pop()
            if not stack:
                return k + 1
    return len(masked)


def attrs_before(masked, at, text):
    """Original text of the `#[...]` groups directly preceding the item at `at`."""
    k = at
    keywords = ("pub", "async", "const", "unsafe", "extern", "default")
    while True:
        j = k
        while j > 0 and masked[j - 1].isspace():
            j -= 1
        if j > 0 and masked[j - 1] == ")":  # pub(crate)
            depth, p = 0, j - 1
            while p >= 0:
                depth += {")": 1, "(": -1}.get(masked[p], 0)
                if depth == 0:
                    break
                p -= 1
            k = p
            continue
        word = re.search(r"(" + IDENT + r")$", masked[max(0, j - 12):j])
        if word and word.group(1) in keywords:
            k = j - len(word.group(1))
            continue
        break
    end = k
    while True:
        j = k
        while j > 0 and masked[j - 1].isspace():
            j -= 1
        if j == 0 or masked[j - 1] != "]":
            break
        depth, p = 0, j - 1
        while p >= 0:
            depth += {"]": 1, "[": -1}.get(masked[p], 0)
            if depth == 0:
                break
            p -= 1
        if p <= 0 or masked[p - 1] != "#":
            break
        k = p - 1
    return text[k:end]


def expand_use(tree):
    """Expand a use-tree into ([segments], alias) pairs; a glob ends in `*`."""
    out = []

    def walk(prefix, t):
        t = t.strip()
        if not t:
            return
        if "{" in t and t.endswith("}"):
            head, body = t.split("{", 1)
            base = prefix + [s.strip() for s in head.strip().rstrip(":").split("::") if s.strip()]
            depth, cur, parts = 0, "", []
            for ch in body[:-1]:
                if ch == "," and depth == 0:
                    parts.append(cur)
                    cur = ""
                    continue
                depth += {"{": 1, "}": -1}.get(ch, 0)
                cur += ch
            parts.append(cur)
            for p in parts:
                walk(base, p)
            return
        alias = None
        if " as " in t:
            t, alias = [x.strip() for x in t.split(" as ", 1)]
        segs = prefix + [s.strip() for s in t.split("::") if s.strip()]
        if segs and segs[-1] == "self":
            segs = segs[:-1]
        if segs:
            out.append((segs, alias))

    walk([], re.sub(r"\s+", " ", tree))
    return out
