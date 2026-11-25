"""extract.py — find every Rust test fn in a source file (#11268).

Why: the verdict log is keyed by test path and re-judges a test only when its
  normalised source changes, so the inventory needs a stable key and a hash
  that a comment-only or whitespace-only edit cannot move but an attribute
  (`#[ignore]`, `#[cfg]`) or signature edit does.
What: `extract_tests(crate, path, src)` walks the token stream, tracks inline
  `mod NAME { … }` nesting, and returns one `TestFn` per fn carrying a test
  attribute: an attribute whose path ends in `test` (`#[test]`,
  `#[tokio::test(…)]`, `#[sqlx::test]`), `#[rstest]` or `#[test_case(…)]`.
  Fns inside `macro_rules!` bodies are templates, not tests; they are counted
  in `FileScan.macro_templates` only. `body_sha` is sha256 over the test's
  attributes, signature and body tokens joined by one space, so comments and
  layout never reach it. `body_only_sha` hashes the body alone: R1 compares
  bodies with it, and it is the value `body_sha` had before #11268 PR0, which
  `verdict_log.migrate_legacy_sha` recognises.
Test: scripts/test_audit/tests/test_lexer_extract.py
"""
from __future__ import annotations

import hashlib
from dataclasses import dataclass, field

from test_audit.lexer import Tok, match_close, str_value, tokenize

TEST_ATTR_LAST = {"test", "rstest", "test_case"}
PROSE_EXTS = (".md", ".markdown", ".txt", ".prompt", ".tmpl", ".hbs", ".j2")


@dataclass
class TestFn:
    crate: str
    path: str
    modpath: list[str]
    name: str
    line: int
    attrs: list[list[Tok]]
    sig: list[Tok]
    body: list[Tok]
    key: str = ""

    @property
    def body_sha(self) -> str:
        """Hash of attributes + signature + body: the verdict log's re-review trigger."""
        return source_sha(self.attrs, self.sig, self.body)

    @property
    def body_only_sha(self) -> str:
        """Hash of the body alone: R1's identity test and the pre-PR0 log value."""
        return body_sha(self.body)


@dataclass
class FileScan:
    tests: list[TestFn] = field(default_factory=list)
    macro_templates: int = 0
    # name -> include_str!/include_bytes! path, for consts, statics and loader fns
    asset_names: dict[str, str] = field(default_factory=dict)
    attr_hits: int = 0  # test-attribute tokens seen in code (not comments or strings)
    helper_bodies: dict[str, list[Tok]] = field(default_factory=dict)  # non-test fns


def body_sha(body: list[Tok]) -> str:
    """sha256 of the normalised body: tokens joined by one space."""
    norm = " ".join(t.text for t in body)
    return hashlib.sha256(norm.encode("utf-8")).hexdigest()


def source_sha(attrs: list[list[Tok]], sig: list[Tok], body: list[Tok]) -> str:
    """sha256 of `#[attr] … sig { body }`, tokens joined by one space.

    Each attribute is re-wrapped in `#[ … ]` so `#[a] #[b]` and `#[a b]` differ.
    """
    texts = [x for a in attrs for x in ("#", "[", *(t.text for t in a), "]")]
    texts += [t.text for t in sig] + ["{", *(t.text for t in body), "}"]
    return hashlib.sha256(" ".join(texts).encode("utf-8")).hexdigest()


def attr_path(attr: list[Tok]) -> list[str]:
    """Leading path segments of an attribute's tokens (`tokio::test(..)` -> [tokio, test])."""
    segs: list[str] = []
    for t in attr:
        if t.kind == "id":
            segs.append(t.text)
        elif not t.is_p("::"):
            break
    return segs


def is_test_attr(attr: list[Tok]) -> bool:
    segs = attr_path(attr)
    return bool(segs) and segs[-1] in TEST_ATTR_LAST and segs[0] != "cfg"


def include_path(toks: list[Tok], at: int) -> str | None:
    """The literal path of an `include_str!(".." )` starting at `at`, else None."""
    if (at + 3 < len(toks) and toks[at].text in ("include_str", "include_bytes")
            and toks[at + 1].is_p("!") and toks[at + 3].kind == "str"):
        return str_value(toks[at + 3].text)
    return None


def _skip_group(toks: list[Tok], i: int) -> int:
    """From token i, skip to just past the next bracket group (or `;`)."""
    while i < len(toks):
        t = toks[i]
        if t.kind == "p" and t.text in "([{":
            return match_close(toks, i) + 1
        if t.is_p(";"):
            return i + 1
        i += 1
    return i


def _fn_body_open(toks: list[Tok], i: int) -> int | None:
    """Index of the `{` opening the fn body that follows `fn NAME` at i, or None for `;`."""
    depth = 0
    while i < len(toks):
        t = toks[i]
        if t.kind == "p":
            if t.text in "([":
                depth += 1
            elif t.text in ")]":
                depth -= 1
            elif depth == 0 and t.text == "{":
                return i
            elif depth == 0 and t.text == ";":
                return None
        i += 1
    return None


def _record_asset_binding(toks: list[Tok], i: int, scan: FileScan) -> None:
    """`const|static NAME: T = include_str!("p")` at i records NAME -> p."""
    if i + 1 >= len(toks) or toks[i + 1].kind != "id":
        return
    name = toks[i + 1].text
    j = i + 2
    while j < len(toks) and not toks[j].is_p("=") and not toks[j].is_p(";"):
        j += 1
    if j + 1 < len(toks) and toks[j].is_p("="):
        p = include_path(toks, j + 1)
        if p is not None:
            scan.asset_names[name] = p


def _count_attr_hits(toks: list[Tok]) -> int:
    """`#[test…` / `#[tokio::test…` in code, matching the `git grep` cross-check regex."""
    hits = 0
    for k in range(len(toks) - 2):
        if toks[k].is_p("#") and toks[k + 1].is_p("["):
            t = toks[k + 2]
            if t.kind == "id" and t.text.startswith("test"):
                hits += 1
            elif (t.is_id("tokio") and k + 4 < len(toks) and toks[k + 3].is_p("::")
                    and toks[k + 4].kind == "id" and toks[k + 4].text.startswith("test")):
                hits += 1
    return hits


def extract_tests(crate: str, path: str, src: str) -> FileScan:
    """Every test fn in one file, in source order, with keys assigned."""
    toks = tokenize(src)
    scan = FileScan(attr_hits=_count_attr_hits(toks))
    stack: list[str | None] = []  # one entry per open `{`: module name or None
    pending: list[list[Tok]] = []
    i, n = 0, len(toks)
    while i < n:
        t = toks[i]
        if t.is_p("#") and i + 1 < n and toks[i + 1].is_p("!"):
            i = _skip_group(toks, i + 2)
            continue
        if t.is_p("#") and i + 1 < n and toks[i + 1].is_p("["):
            close = match_close(toks, i + 1)
            pending.append(toks[i + 2:close])
            i = close + 1
            continue
        if t.is_id("macro_rules") and i + 1 < n and toks[i + 1].is_p("!"):
            j = i + 2
            while j < n and not (toks[j].kind == "p" and toks[j].text in "({["):
                j += 1
            end = match_close(toks, j) if j < n else n
            inner = toks[j:end]
            scan.macro_templates += sum(
                1 for k in range(len(inner) - 1)
                if inner[k].is_p("#") and inner[k + 1].is_p("[")
                and is_test_attr(inner[k + 2:match_close(inner, k + 1)]))
            pending = []
            i = end + 1
            continue
        if t.is_id("mod") and i + 2 < n and toks[i + 1].kind == "id" and toks[i + 2].is_p("{"):
            stack.append(toks[i + 1].text)
            pending = []
            i += 3
            continue
        if t.kind == "id" and t.text in ("const", "static"):
            _record_asset_binding(toks, i, scan)
        if t.is_id("fn") and i + 1 < n and toks[i + 1].kind == "id":
            name = toks[i + 1].text
            open_at = _fn_body_open(toks, i + 2)
            if open_at is None:
                pending = []
                i += 2
                continue
            close = match_close(toks, open_at)
            body = toks[open_at + 1:close]
            if any(is_test_attr(a) for a in pending):
                mods = [m for m in stack if m is not None]
                scan.tests.append(TestFn(crate, path, mods, name, t.line, pending,
                                         toks[i:open_at], body))
            elif body:
                scan.helper_bodies.setdefault(name, body)
                p = include_path(body, 0)
                if p is not None and len(body) <= 6:
                    scan.asset_names[name] = p
            pending = []
            i = close + 1
            continue
        if t.kind == "p" and t.text in "{};":
            pending = []
            if t.text == "{":
                stack.append(None)
            elif t.text == "}" and stack:
                stack.pop()
        i += 1
    assign_keys(scan.tests)
    return scan


def assign_keys(tests: list[TestFn]) -> None:
    """`<crate>::<file>::<mod path>::<fn>`; an empty mod path drops its segment.

    A repeated key in one file (cfg-gated twins) gets `#2`, `#3`, … in source order.
    """
    seen: dict[str, int] = {}
    for t in tests:
        parts = [t.crate, t.path, *t.modpath, t.name]
        base = "::".join(parts)
        seen[base] = seen.get(base, 0) + 1
        t.key = base if seen[base] == 1 else f"{base}#{seen[base]}"


def key_file(key: str) -> str:
    """The repo-relative file segment of a key."""
    return key.split("::")[1]
