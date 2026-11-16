"""Optional controls of the read-only page queries; contract docs/activation-codes.md.

Split from pages.py for the SLOC cap. Tests live in tests/test_page_ops.py.
"""

from __future__ import annotations

from trusty_browser.protocol import Frame, Refusal

# #11356: page_read's controls; each absent one takes its default.
READ_DEFAULTS: Frame = {
    "scope_ref": None,
    "max_chars": 50000,
    "viewport_only": False,
    "include_text": False,
}
DEFAULTS: dict[str, Frame] = {"page_read": READ_DEFAULTS}


def optional(op: str) -> set[str]:
    """The optional field names of ``op``."""
    return set(DEFAULTS.get(op, {}))


def controls(request: Frame) -> Frame:
    """The op's controls with defaults filled, else ``InvalidRequest``.

    Why: the worker refuses a malformed control, so the broker refuses it first
    and sends nothing. What: fills defaults, then checks each control's type
    and range as the worker's SPECS do.
    Test: `test_page_read_sends_its_controls_and_refuses_bad_ones`.
    """
    fields = {**DEFAULTS.get(str(request["op"]), {})}
    fields.update({k: request[k] for k in fields if k in request})
    ref = fields.get("scope_ref")
    if ref is not None and not (isinstance(ref, str) and 0 < len(ref) <= 8192):
        raise Refusal("InvalidRequest")
    if "max_chars" in fields:
        size = fields["max_chars"]
        if type(size) is not int or not 1000 <= size <= 250000:
            raise Refusal("InvalidRequest")
    for name in ("viewport_only", "include_text"):
        if name in fields and type(fields[name]) is not bool:
            raise Refusal("InvalidRequest")
    return fields
