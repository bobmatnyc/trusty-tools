"""Optional controls of the page queries and page_click; contract docs/activation-codes.md.

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
# #11357: page_find's terms; at least one of role, name and text is set.
FIND_DEFAULTS: Frame = {
    "role": None,
    "name": None,
    "text": None,
    "within_ref": None,
    "limit": 10,
}
# #11294: how a trusted click answers a native dialog it opens.
CLICK_DEFAULTS: Frame = {"dialog": "dismiss"}
DEFAULTS: dict[str, Frame] = {
    "page_read": READ_DEFAULTS,
    "page_find": FIND_DEFAULTS,
    "page_click": CLICK_DEFAULTS,
}
DIALOG_TYPES = {"alert", "confirm", "prompt", "beforeunload"}
TERMS = ("role", "name", "text")


def optional(op: str) -> set[str]:
    """The optional field names of ``op``."""
    return set(DEFAULTS.get(op, {}))


def controls(request: Frame) -> Frame:
    """The op's controls with defaults filled, else ``InvalidRequest``.

    Why: the worker refuses a malformed control, so the broker refuses it first
    and sends nothing. What: fills defaults, then checks each control's type
    and range as the worker's SPECS do.
    Test: `test_page_read_sends_its_controls_and_refuses_bad_ones`,
    `test_page_find_relays_matches_and_refuses_bad_terms`.
    """
    fields = {**DEFAULTS.get(str(request["op"]), {})}
    fields.update({k: request[k] for k in fields if k in request})
    for name in ("scope_ref", "within_ref"):
        ref = fields.get(name)
        if ref is not None and not (isinstance(ref, str) and 0 < len(ref) <= 8192):
            raise Refusal("InvalidRequest")
    for name in TERMS:
        term = fields.get(name)
        if term is not None and not (isinstance(term, str) and 0 < len(term) <= 200):
            raise Refusal("InvalidRequest")
    if "limit" in fields:
        limit = fields["limit"]
        if type(limit) is not int or not 1 <= limit <= 50:
            raise Refusal("InvalidRequest")
        if all(fields[name] is None for name in TERMS):
            raise Refusal("InvalidRequest")
    if "max_chars" in fields:
        size = fields["max_chars"]
        if type(size) is not int or not 1000 <= size <= 250000:
            raise Refusal("InvalidRequest")
    for name in ("viewport_only", "include_text"):
        if name in fields and type(fields[name]) is not bool:
            raise Refusal("InvalidRequest")
    if fields.get("dialog", "dismiss") not in ("accept", "dismiss"):
        raise Refusal("InvalidRequest")
    return fields


def clicked(raw: object) -> Frame:
    """A click result: ``document``, plus ``dialog`` when one opened, else ``BrowserRefused``.

    Test: `test_dialog_policy_is_forwarded_and_the_dialog_relayed`.
    """
    if not isinstance(raw, dict) or set(raw) - {"dialog"} != {"document"}:
        raise Refusal("BrowserRefused")
    if type(raw["document"]) is not str:
        raise Refusal("BrowserRefused")
    seen = raw.get("dialog", {"type": "alert", "message": "", "accepted": True})
    kinds = {"type": str, "message": str, "accepted": bool}
    if not isinstance(seen, dict) or set(seen) != set(kinds):
        raise Refusal("BrowserRefused")
    if any(type(seen[k]) is not v for k, v in kinds.items()):
        raise Refusal("BrowserRefused")
    if seen["type"] not in DIALOG_TYPES or len(seen["message"]) > 1024:
        raise Refusal("BrowserRefused")
    return raw


# Worker refusals after which the lease stays; any other drops it.
KEEP_LEASE = {
    "PermissionNeeded",
    "DocumentChanged",
    "NavigationPending",
    "StaleElement",
    "NotEditable",
    "TextDropped",
    "NotSelectable",
    "NoSuchOption",
    "Obscured",
    "DebuggerBusy",
    "NotDraggable",
    "InvalidUrl",
    "InvalidRequest",
    "InvalidOperation",
    "NoHistory",
}
