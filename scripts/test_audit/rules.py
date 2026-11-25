"""rules.py — the deterministic candidate rules R2-R6 of the test audit (#11268).

Why: Phase 0 flags candidates for review without a model; every rule must be
  reproducible from source text alone so a re-run fires the same set.
What: `per_test_rules(test, asset_names)` returns `{rule_id: detail}` for one
  test. R1 (duplicates) needs every test at once and lives in dupes.py.
  R2 text pin: a compared assertion argument references embedded prose — an
     `include_str!` of a .md/.txt/.prompt-style file (directly, through a
     file-level const/static/loader fn, or through a local bound from one), or
     a string literal of 8+ words or carrying markdown markup that the
     assertion compares against (top level of a compared argument, or inside
     `.contains(…)`-style matchers) — never an input passed to the code.
  R3 no behavioural assertion: no assertion of any kind, or only
     `assert!(x.is_ok())` / `assert!(true)`; `?`, `unwrap()` and "does not
     panic" are not assertions. `#[should_panic]` counts as one.
  R4 mock-only: the test binds a mock/stub/fake/spy (a `let` whose head
     constructor, or whose own name, has such a word part) and every
     assertion is a mock expectation or compares that binding.
  R5 constant/derive: every assertion compares only literals and constants,
     only fields of a `Default` value, or the two sides of a serde round trip.
  R6 `#[ignore]` with no reason string, or an `intent(demo)` ignore.
Test: scripts/test_audit/tests/test_rules.py
"""
from __future__ import annotations

import re

from test_audit.assertions import Assertion, scan_assertions, strong
from test_audit.extract import PROSE_EXTS, TestFn, attr_path, include_path
from test_audit.lexer import Tok, split_args, str_value

MOCK_PARTS = {"mock", "mocks", "stub", "stubs", "fake", "fakes", "spy", "spies"}
WRAPPERS = {"Arc", "Rc", "Box", "Mutex", "RwLock", "RefCell", "Cell", "Some", "Ok"}
NAME_PARTS = {"mock", "mocks", "stub", "stubs", "spy", "spies"}  # a `fake_home` dir is no double
TRIVIAL_METHODS = {"len", "is_empty", "is_none", "is_some", "as_str", "as_deref",
                   "to_string", "to_owned", "clone", "unwrap", "into", "as_ref"}
LITERAL_IDS = {"true", "false", "None", "Some", "Ok", "Err", "as", "u8", "u16", "u32",
               "u64", "usize", "i32", "i64", "f32", "f64", "String", "from"}
SER = {"to_string", "to_string_pretty", "to_vec", "to_vec_pretty", "to_value", "to_writer"}
DE = {"from_str", "from_slice", "from_value", "from_reader"}
SERDE_CRATES = {"serde_json", "toml", "serde_yaml", "bincode", "postcard", "rmp_serde",
                "ciborium", "serde_cbor", "ron"}
MARKDOWN = re.compile(r"(^|\n)\s*#{1,6} |\*\*|```|\]\(|(^|\n)\s*[-*] \S")
PROSE_WORDS = 8
TEXT_MATCHERS = {"contains", "starts_with", "ends_with", "find", "rfind", "eq",
                 "strip_prefix", "strip_suffix", "matches", "Some", "Ok", "Err"}


def word_parts(ident: str) -> list[str]:
    """`MockKeychain` -> [mock, keychain]; `fake_home` -> [fake, home]."""
    spaced = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", ident)
    return [p.lower() for p in spaced.split("_") if p]


def is_mockish(ident: str) -> bool:
    return any(p in MOCK_PARTS for p in word_parts(ident))


def let_bindings(body: list[Tok]) -> list[tuple[str, list[Tok]]]:
    """`let [mut] NAME [: T] = EXPR;` pairs at any depth (patterns are skipped)."""
    out: list[tuple[str, list[Tok]]] = []
    for i, t in enumerate(body):
        if not t.is_id("let"):
            continue
        j = i + 1
        if j < len(body) and body[j].is_id("mut"):
            j += 1
        if j >= len(body) or body[j].kind != "id":
            continue
        name = body[j].text
        k, depth = j + 1, 0
        while k < len(body) and not (depth == 0 and body[k].is_p("=")):
            if body[k].kind == "p" and body[k].text in "([{<":
                depth += 1
            elif body[k].kind == "p" and body[k].text in ")]}>":
                depth -= 1
            if body[k].is_p(";"):
                break
            k += 1
        if k >= len(body) or not body[k].is_p("="):
            continue
        e, depth = k + 1, 0
        while e < len(body) and not (depth == 0 and body[e].is_p(";")):
            if body[e].kind == "p" and body[e].text in "([{":
                depth += 1
            elif body[e].kind == "p" and body[e].text in ")]}":
                depth -= 1
            e += 1
        out.append((name, body[k + 1:e]))
    return out


def head_idents(expr: list[Tok]) -> list[str]:
    """Path idents of the head call, looking through `Arc::new(..)`-style wrappers.

    `MockStore::new()` -> [MockStore, new]; `Arc::new(FakeX::default())` ->
    [Arc, new, FakeX, default]; `Service::new(MockRepo::new())` -> [Service, new].
    """
    heads: list[str] = []
    path: list[str] = []
    for t in expr:
        if t.is_id("mut") or (t.kind == "p" and t.text in ("::", "&")):
            continue
        if t.kind == "id":
            path.append(t.text)
            heads.append(t.text)
            continue
        if t.is_p("(") and path and path[0] in WRAPPERS:
            path = []
            continue
        break
    return heads


def _idents(cond: list[Tok]) -> set[str]:
    return {t.text for t in cond if t.kind == "id"}


# ---------------------------------------------------------------- R2
def compared_literals(cond: list[Tok]) -> list[Tok]:
    """String literals the assertion compares against, not inputs to the code under test.

    A literal counts at the top level of a compared argument, or as the argument
    of a text-matching method (`.contains("…")`, `.starts_with("…")`, …).
    """
    out: list[Tok] = []
    callers: list[str | None] = []
    for k, t in enumerate(cond):
        if t.kind == "p" and t.text in "([{":
            prev = cond[k - 1] if k else None
            callers.append(prev.text if prev is not None and prev.kind == "id" else None)
        elif t.kind == "p" and t.text in ")]}":
            if callers:
                callers.pop()
        elif t.kind == "str" and (not callers or callers[-1] in TEXT_MATCHERS):
            out.append(t)
    return out


def _prose_literal(t: Tok) -> bool:
    if t.kind != "str":
        return False
    value = str_value(t.text)
    return len(value.split()) >= PROSE_WORDS or bool(MARKDOWN.search(value.replace("\\n", "\n")))


def rule_r2(test: TestFn, asserts: list[Assertion], asset_names: dict[str, str]) -> str | None:
    prose_names = {name for name, p in asset_names.items() if p.lower().endswith(PROSE_EXTS)}
    body = test.body
    inline = [include_path(body, k) for k in range(len(body))]
    inline_prose = [p for p in inline if p and p.lower().endswith(PROSE_EXTS)]
    for _ in range(2):  # locals bound from an asset, then locals bound from those
        for name, expr in let_bindings(body):
            ids = _idents(expr)
            if ids & prose_names or any(include_path(expr, k) and include_path(expr, k).lower()
                                        .endswith(PROSE_EXTS) for k in range(len(expr))):
                prose_names.add(name)
    for a in strong(asserts):
        if a.kind != "macro" or not a.known_args:
            continue
        hit = _idents(a.cond) & prose_names
        if hit or (inline_prose and "include_str" in _idents(a.cond)):
            src = sorted(hit)[0] if hit else "include_str!"
            return f"asserts on embedded text ({src})"
        for t in compared_literals(a.cond):
            if _prose_literal(t):
                return "asserts on a prose string literal"
    return None


# ---------------------------------------------------------------- R3
def rule_r3(asserts: list[Assertion]) -> str | None:
    if not asserts:
        return "no assertion"
    if not strong(asserts):
        return "only weak assertions (is_ok()/true)"
    return None


# ---------------------------------------------------------------- R4
def mock_bound(body: list[Tok]) -> set[str]:
    names: set[str] = set()
    for name, expr in let_bindings(body):
        heads = head_idents(expr)
        named = any(p in NAME_PARTS for p in word_parts(name))
        if any(is_mockish(h) for h in heads) or named:
            names.add(name)
    return names


def _lead_ident(arg: list[Tok]) -> str | None:
    """First identifier of an argument, past `&`, `*`, `!`, `(` and `mut`."""
    for t in arg:
        if t.kind == "p" and t.text in ("&", "*", "!", "("):
            continue
        if t.is_id("mut"):
            continue
        return t.text if t.kind == "id" else None
    return None


def _on_mock(a: Assertion, mocks: set[str]) -> bool:
    """Some compared argument starts at a mock (`fake.calls()`), not a call taking one."""
    for arg in split_args(a.cond):
        lead = _lead_ident(arg)
        if lead is not None and (lead in mocks or is_mockish(lead)):
            return True
    return False


def rule_r4(test: TestFn, asserts: list[Assertion]) -> str | None:
    mocks = mock_bound(test.body)
    checks = [a for a in strong(asserts) if a.kind != "branch"]
    if not checks or not (mocks or any(a.kind == "mock" for a in checks)):
        return None
    for a in checks:
        if a.kind != "mock" and not _on_mock(a, mocks):
            return None
    return f"every assertion is on a test-built mock ({', '.join(sorted(mocks)) or 'expectations'})"


# ---------------------------------------------------------------- R5
def _const_only(cond: list[Tok]) -> bool:
    for k, t in enumerate(cond):
        if t.kind != "id":
            continue
        nxt = cond[k + 1] if k + 1 < len(cond) else None
        prev = cond[k - 1] if k else None
        if nxt is not None and nxt.is_p("::"):
            continue
        if prev is not None and prev.is_p("."):
            if t.text in TRIVIAL_METHODS:
                continue
            return False
        if t.text in LITERAL_IDS:
            continue
        if t.text.isupper() or (t.text[:1].isupper() and not (nxt and nxt.is_p("("))):
            continue
        return False
    return True


def _default_only(cond: list[Tok], defaults: set[str]) -> bool:
    uses_default = False
    for k, t in enumerate(cond):
        if t.kind != "id":
            continue
        nxt = cond[k + 1] if k + 1 < len(cond) else None
        prev = cond[k - 1] if k else None
        if t.text in defaults or t.text == "default":
            uses_default = True
            continue
        if nxt is not None and nxt.is_p("::"):
            continue
        if prev is not None and prev.is_p("."):
            if nxt is not None and nxt.is_p("(") and t.text not in TRIVIAL_METHODS:
                return False
            continue
        if t.text in LITERAL_IDS or t.text.isupper() or (t.text[:1].isupper()
                                                          and not (nxt and nxt.is_p("("))):
            continue
        return False
    return uses_default


def _roundtrip_pair(a: Assertion) -> bool:
    """`assert_eq!(back, original)`: one ident per side, no call but `.clone()`."""
    if a.name not in ("assert_eq", "debug_assert_eq"):
        return False
    ids = [t for t in a.cond if t.kind == "id" and t.text != "clone"]
    calls = [a.cond[k - 1].text for k, t in enumerate(a.cond) if t.is_p("(") and k]
    return len(ids) == 2 and all(c == "clone" for c in calls)


def _serde_calls(body: list[Tok]) -> tuple[bool, bool]:
    ser = de = False
    for k in range(2, len(body)):
        t = body[k]
        if t.kind == "id" and body[k - 1].is_p("::") and body[k - 2].text in SERDE_CRATES:
            ser |= t.text in SER
            de |= t.text in DE
    return ser, de


def rule_r5(test: TestFn, asserts: list[Assertion]) -> str | None:
    checks = [a for a in strong(asserts) if a.kind != "branch"]
    if not checks or any(a.kind != "macro" or not a.known_args for a in checks):
        return None
    defaults = {name for name, expr in let_bindings(test.body)
                if [t.text for t in expr[-3:]] == ["default", "(", ")"]}
    ser, de = _serde_calls(test.body)
    kinds: set[str] = set()
    for a in checks:
        if _const_only(a.cond):
            kinds.add("literal/constant")
        elif _default_only(a.cond, defaults):
            kinds.add("Default value")
        elif ser and de and _roundtrip_pair(a):
            kinds.add("serde round trip")
        else:
            return None
    return "asserts only " + " + ".join(sorted(kinds))


# ---------------------------------------------------------------- R6
def rule_r6(test: TestFn) -> str | None:
    for a in test.attrs:
        if attr_path(a) != ["ignore"]:
            continue
        reason = next((str_value(t.text) for t in a if t.kind == "str"), None)
        if reason is None:
            return "#[ignore] without a reason"
        if "intent(demo)" in reason:
            return "intent(demo) ignore"
    return None


def per_test_rules(test: TestFn, asset_names: dict[str, str],
                   helpers: frozenset[str] = frozenset()) -> dict[str, str]:
    asserts = scan_assertions(test.body, test.attrs, helpers)
    found = {
        "R2": rule_r2(test, asserts, asset_names),
        "R3": rule_r3(asserts),
        "R4": rule_r4(test, asserts),
        "R5": rule_r5(test, asserts),
        "R6": rule_r6(test),
    }
    return {k: v for k, v in found.items() if v}
