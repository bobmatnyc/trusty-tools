"""Fields and reply shapes of page_styles and page_evaluate (#11373); contract docs/activation-codes.md.

Split from pages.py for the SLOC cap. Tests live in tests/test_page_ops.py.
"""

from __future__ import annotations

import re

from trusty_browser.protocol import Frame, Refusal

# page_styles names its element by exactly one of a page_read ref and a CSS selector.
STYLES_DEFAULTS: Frame = {"ref": None, "selector": None}
# A CSS property or custom property name, as getPropertyValue takes it.
PROPERTY = re.compile(r"(--|-)?[A-Za-z][A-Za-z0-9-]*")
MAX_PROPERTIES = 50
MAX_EXPRESSION = 8192
# The longest JSON text of an evaluate result the worker relays.
MAX_VALUE = 65536


def check(request: Frame) -> None:
    """Refuse a malformed page_styles or page_evaluate frame with ``InvalidRequest``.

    Why: the worker refuses the same frames, so the broker refuses them first
    and sends nothing (#11373). What: page_styles takes 1-50 distinct property
    names of at most 100 characters and exactly one of ``ref`` (str) and
    ``selector`` (1-1024 characters); page_evaluate takes a 1-8192 character
    ``expression``.
    Test: `test_page_styles_refuses_bad_frames_and_sends_nothing`,
    `test_page_evaluate_refuses_bad_expressions_and_sends_nothing`.
    """
    op = request["op"]
    if op == "page_evaluate":
        expression = request["expression"]
        if not isinstance(expression, str) or not 0 < len(expression) <= MAX_EXPRESSION:
            raise Refusal("InvalidRequest")
        return
    if op != "page_styles":
        return
    names = request["properties"]
    if not (
        isinstance(names, list)
        and 0 < len(names) <= MAX_PROPERTIES
        and len(set(map(str, names))) == len(names)
        and all(
            isinstance(n, str) and len(n) <= 100 and PROPERTY.fullmatch(n)
            for n in names
        )
    ):
        raise Refusal("InvalidRequest")
    ref, selector = request.get("ref"), request.get("selector")
    if (ref is None) == (selector is None):
        raise Refusal("InvalidRequest")
    if ref is not None and not (isinstance(ref, str) and 0 < len(ref) <= 8192):
        raise Refusal("InvalidRequest")
    if selector is not None and not (
        isinstance(selector, str) and 0 < len(selector) <= 1024
    ):
        raise Refusal("InvalidRequest")


def styled(raw: object, names: list[str]) -> Frame:
    """A page_styles result: ``document`` and one string per requested property, else ``BrowserRefused``.

    Test: `test_page_styles_relays_computed_values_for_a_ref_or_a_selector`.
    """
    if not isinstance(raw, dict) or set(raw) != {"document", "styles"}:
        raise Refusal("BrowserRefused")
    styles = raw["styles"]
    if type(raw["document"]) is not str or not isinstance(styles, dict):
        raise Refusal("BrowserRefused")
    if set(styles) != set(names) or any(type(v) is not str for v in styles.values()):
        raise Refusal("BrowserRefused")
    return raw


EVALUATED = {
    "document": (str,),
    "type": (str,),
    "value": (str, type(None)),
    "exception": (str, type(None)),
    "truncated": (bool,),
}


def evaluated(raw: object) -> Frame:
    """A page_evaluate result in the contract's shape, else ``BrowserRefused``.

    Test: `test_page_evaluate_relays_the_value_journals_and_audits`.
    """
    if not isinstance(raw, dict) or set(raw) != set(EVALUATED):
        raise Refusal("BrowserRefused")
    if any(type(raw[k]) not in kinds for k, kinds in EVALUATED.items()):
        raise Refusal("BrowserRefused")
    value, exception = raw["value"], raw["exception"]
    if (value is not None and len(value) > MAX_VALUE) or (
        exception is not None and len(exception) > 1024
    ):
        raise Refusal("BrowserRefused")
    return raw
