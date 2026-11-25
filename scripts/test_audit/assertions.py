"""assertions.py — find and classify the assertions inside a test body (#11268).

Why: rules R2-R5 all ask what a test asserts on, so the assertion scan lives
  once here and the rules read its result.
What: `scan_assertions(body, attrs)` returns `Assertion` items:
  - `macro`: an `assert*!`, `debug_assert*!`, `prop_assert*!` or `ensure!`
    invocation. `cond` holds only the compared arguments: the first for
    `assert!`, the first two for `*_eq!`/`*_ne!`/`assert_matches!`; message
    arguments are dropped. A custom `assert_*!` macro keeps every argument.
  - `branch`: a `panic!`/`unreachable!` (a hand-rolled match-arm check).
  - `err`: `.unwrap_err()` / `.expect_err(` (the test asserts a failure).
  - `helper`: a call to a fn named `assert…` (`assert_ok(x)`, `.assert()`).
  - `helper`: also a call to a file-local fn that itself asserts
    (`asserting_helpers`), e.g. `kept_agent_rules(&fx, …)`.
  - `mock`: a mockall-style `.expect_<method>(` expectation.
  `weak` marks an `assert!(<expr>.is_ok())` or `assert!(true)` (R3).
  `#[should_panic]` on the fn is reported as one `branch` assertion.
Test: scripts/test_audit/tests/test_rules.py
"""
from __future__ import annotations

from dataclasses import dataclass

from test_audit.extract import attr_path
from test_audit.lexer import Tok, match_close, split_args

TWO_ARG_SUFFIXES = ("_eq", "_ne", "_matches")


@dataclass
class Assertion:
    kind: str  # macro | branch | err | helper | mock
    name: str
    cond: list[Tok]
    line: int
    weak: bool = False
    known_args: bool = True  # False: a custom macro or helper whose message args are unknown


def _is_assert_macro(name: str) -> bool:
    return (name.startswith("assert") or name.startswith("debug_assert")
            or name.startswith("prop_assert") or name == "ensure")


def _cond_args(name: str, args: list[list[Tok]]) -> tuple[list[Tok], bool]:
    base = name.removeprefix("debug_").removeprefix("prop_")
    if base in ("assert", "ensure"):
        keep, known = args[:1], True
    elif base.startswith("assert") and base.endswith(TWO_ARG_SUFFIXES):
        keep, known = args[:2], True
    else:
        keep, known = args, False
    flat: list[Tok] = []
    for k, a in enumerate(keep):
        if k:
            flat.append(Tok("p", ",", a[0].line))
        flat.extend(a)
    return flat, known


def _is_weak(name: str, cond: list[Tok]) -> bool:
    if name not in ("assert", "debug_assert"):
        return False
    texts = [t.text for t in cond]
    if texts == ["true"]:
        return True
    if any(t.kind == "p" and t.text in ("&&", "||", "==", "!=", "!") for t in cond):
        return False
    return texts[-4:] == [".", "is_ok", "(", ")"]


def asserting_helpers(helper_bodies: dict[str, list[Tok]]) -> frozenset[str]:
    """File-local non-test fns that assert, directly or through another such fn (fixed point)."""
    found: set[str] = set()
    while True:
        more = {name for name, body in helper_bodies.items() if name not in found
                and strong(scan_assertions(body, [], frozenset(found)))}
        if not more:
            return frozenset(found)
        found |= more


def scan_assertions(body: list[Tok], attrs: list[list[Tok]],
                    helpers: frozenset[str] = frozenset()) -> list[Assertion]:
    out: list[Assertion] = []
    if any(attr_path(a)[-1:] == ["should_panic"] for a in attrs):
        out.append(Assertion("branch", "should_panic", [], 0))
    i, n = 0, len(body)
    while i < n:
        t = body[i]
        nxt = body[i + 1] if i + 1 < n else None
        if t.kind == "id" and nxt is not None and nxt.is_p("!") and i + 2 < n \
                and body[i + 2].kind == "p" and body[i + 2].text in "([{":
            close = match_close(body, i + 2)
            if _is_assert_macro(t.text):
                cond, known = _cond_args(t.text, split_args(body[i + 3:close]))
                out.append(Assertion("macro", t.text, cond, t.line,
                                     weak=_is_weak(t.text, cond), known_args=known))
                i = close + 1
                continue
            if t.text in ("panic", "unreachable"):
                out.append(Assertion("branch", t.text, [], t.line))
                i = close + 1
                continue
            i += 3  # other macros (vec!, format!, matches!): look inside
            continue
        if (t.kind == "id" and t.text.startswith("assert") and nxt is not None
                and nxt.is_p("::") and i + 2 < n and body[i + 2].is_p("<")):
            out.append(Assertion("helper", t.text, [], t.line, known_args=False))  # turbofish
        if t.kind == "id" and nxt is not None and nxt.is_p("("):
            prev = body[i - 1] if i else None
            is_method = prev is not None and prev.is_p(".")
            if t.text in ("unwrap_err", "expect_err") and is_method:
                out.append(Assertion("err", t.text, [], t.line))
            elif t.text.startswith("assert"):
                close = match_close(body, i + 1)
                cond = body[i + 2:close]
                if is_method and i >= 2:
                    cond = [body[i - 2], *cond]
                out.append(Assertion("helper", t.text, cond, t.line, known_args=False))
            elif not is_method and t.text in helpers:
                close = match_close(body, i + 1)
                out.append(Assertion("helper", t.text, body[i + 2:close], t.line,
                                     known_args=False))
            elif is_method and t.text.startswith("expect_") and t.text != "expect_err":
                out.append(Assertion("mock", t.text, [], t.line))
        i += 1
    return out


def strong(assertions: list[Assertion]) -> list[Assertion]:
    return [a for a in assertions if not a.weak]
