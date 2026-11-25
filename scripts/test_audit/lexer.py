"""lexer.py — a Rust token stream for the test-audit inventory (#11268).

Why: body_sha must ignore comments and whitespace but keep string-literal
  content, and the rules need tokens with line numbers. scripts/lib/
  asset_filter_lex.py blanks literal bodies instead, so it cannot serve here.
What: `tokenize(src)` returns `Tok(kind, text, line)` items. Kinds: `id`
  (identifier or keyword), `str` (any string literal, prefix and quotes kept
  in `text`), `chr` (char or byte literal), `num`, `life` (lifetime or label)
  and `p` (punctuation). Comments, doc comments included, produce no token.
Test: scripts/test_audit/tests/test_lexer_extract.py
"""
from __future__ import annotations

import re
from dataclasses import dataclass

PUNCT2 = ("::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "..")
RAW_START = re.compile(r'(?:b|c)?r(#*)"')
IDENT_RE = re.compile(r"[^\W\d]\w*")
NUM_RE = re.compile(r"\d[\w]*(?:\.\d[\w]*)?")


@dataclass(frozen=True)
class Tok:
    kind: str
    text: str
    line: int

    def is_p(self, text: str) -> bool:
        return self.kind == "p" and self.text == text

    def is_id(self, text: str) -> bool:
        return self.kind == "id" and self.text == text


def str_value(text: str) -> str:
    """The content of a string-literal token, without prefix, hashes and quotes."""
    start = text.index('"')
    hashes = start - text.find("r") - 1 if "r" in text[:start] else 0
    return text[start + 1: len(text) - 1 - max(hashes, 0)]


def _skip_block_comment(src: str, i: int) -> int:
    depth, j, n = 1, i + 2, len(src)
    while j < n and depth:
        if src.startswith("/*", j):
            depth, j = depth + 1, j + 2
        elif src.startswith("*/", j):
            depth, j = depth - 1, j + 2
        else:
            j += 1
    return j


def _quoted_end(src: str, j: int) -> int:
    """Index just past the closing `"` of a cooked string whose body starts at `j`."""
    n = len(src)
    while j < n and src[j] != '"':
        j += 2 if src[j] == "\\" else 1
    return min(j + 1, n)


def tokenize(src: str) -> list[Tok]:
    """Split Rust source into tokens; comments are dropped, literals kept whole."""
    toks: list[Tok] = []
    i, n, line = 0, len(src), 1
    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            i += 1
            continue
        if c.isspace():
            i += 1
            continue
        if src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
            continue
        if src.startswith("/*", i):
            j = _skip_block_comment(src, i)
            line += src.count("\n", i, j)
            i = j
            continue
        start = i
        if c in "bcr":
            m = RAW_START.match(src, i)
            if m:
                close = '"' + m.group(1)
                j = src.find(close, m.end())
                j = n if j < 0 else j + len(close)
                toks.append(Tok("str", src[i:j], line))
                line += src.count("\n", i, j)
                i = j
                continue
            if c in "bc" and src.startswith('"', i + 1):
                j = _quoted_end(src, i + 2)
                toks.append(Tok("str", src[i:j], line))
                line += src.count("\n", i, j)
                i = j
                continue
            if c == "b" and src.startswith("'", i + 1):
                j = src.find("'", i + 3 if src.startswith("\\", i + 2) else i + 2)
                j = n if j < 0 else j + 1
                toks.append(Tok("chr", src[i:j], line))
                i = j
                continue
            if c == "r" and src.startswith("#", i + 1):
                m = IDENT_RE.match(src, i + 2)
                if m:  # raw identifier r#type
                    toks.append(Tok("id", m.group(0), line))
                    i = m.end()
                    continue
        if c == '"':
            j = _quoted_end(src, i + 1)
            toks.append(Tok("str", src[i:j], line))
            line += src.count("\n", i, j)
            i = j
            continue
        if c == "'":
            if src.startswith("\\", i + 1):
                j = src.find("'", i + 3)
                j = n if j < 0 else j + 1
                toks.append(Tok("chr", src[i:j], line))
                i = j
                continue
            if i + 2 < n and src[i + 2] == "'":
                toks.append(Tok("chr", src[i:i + 3], line))
                i += 3
                continue
            m = IDENT_RE.match(src, i + 1)
            end = m.end() if m else i + 1
            toks.append(Tok("life", src[i:end], line))
            i = end
            continue
        if c.isdigit():
            m = NUM_RE.match(src, i)
            toks.append(Tok("num", m.group(0), line))
            i = m.end()
            continue
        m = IDENT_RE.match(src, i)
        if m:
            toks.append(Tok("id", m.group(0), line))
            i = m.end()
            continue
        if src.startswith("..=", i):
            toks.append(Tok("p", "..=", line))
            i += 3
            continue
        two = src[i:i + 2]
        if two in PUNCT2:
            toks.append(Tok("p", two, line))
            i += 2
            continue
        toks.append(Tok("p", src[start], line))
        i += 1
    return toks


def match_close(toks: list[Tok], open_at: int) -> int:
    """Index of the token closing the bracket at `open_at` (len(toks) if unclosed)."""
    pairs = {"{": "}", "(": ")", "[": "]"}
    stack: list[str] = []
    for k in range(open_at, len(toks)):
        t = toks[k]
        if t.kind != "p":
            continue
        if t.text in pairs:
            stack.append(pairs[t.text])
        elif stack and t.text == stack[-1]:
            stack.pop()
            if not stack:
                return k
    return len(toks)


def split_args(toks: list[Tok]) -> list[list[Tok]]:
    """Split a bracket group's inner tokens at top-level commas."""
    args: list[list[Tok]] = [[]]
    depth = 0
    for t in toks:
        if t.kind == "p" and t.text in "([{":
            depth += 1
        elif t.kind == "p" and t.text in ")]}":
            depth -= 1
        if depth == 0 and t.is_p(","):
            args.append([])
            continue
        args[-1].append(t)
    return [a for a in args if a]
