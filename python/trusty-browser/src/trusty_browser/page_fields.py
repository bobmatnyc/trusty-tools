"""Field validators for Broker page ops (upload files, key presses).

Split from pages.py to hold the 500-SLOC cap (#11292); the contract is
docs/activation-codes.md "Field types". Tests live in tests/test_page_ops.py.
"""

from __future__ import annotations

import base64
import binascii
import unicodedata

# #11289: the worker's own limits; base64 of MAX_UPLOAD bytes fits one 1 MiB native frame.
MAX_FILES = 10
MAX_UPLOAD = 512 * 1024
FILE_KEYS = {"name", "type", "data"}


def valid_files(files: object) -> bool:
    """True for 1-10 ``{name, type, data}`` entries whose base64 decodes within ``MAX_UPLOAD``."""
    if not isinstance(files, list) or not 0 < len(files) <= MAX_FILES:
        return False
    total = 0
    for item in files:
        if not isinstance(item, dict) or set(item) != FILE_KEYS:
            return False
        name, kind, data = item["name"], item["type"], item["data"]
        if not isinstance(name, str) or not 0 < len(name) <= 255:
            return False
        if "/" in name or "\\" in name:
            return False
        if not isinstance(kind, str) or len(kind) > 255 or not isinstance(data, str):
            return False
        try:
            total += len(base64.b64decode(data, validate=True))
        except (binascii.Error, ValueError):
            return False
    return total <= MAX_UPLOAD


# #11286: the keys page_press names; any other single printable character is also a key.
NAMED_KEYS = {
    "Enter", "Tab", "Escape", "Backspace", "Delete", "ArrowUp", "ArrowDown",
    "ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown", "Space", " ",
}  # fmt: skip
MODIFIERS = {"Shift", "Control", "Alt", "Meta"}


def key_allowed(key: object) -> bool:
    """A named key, or one code point that is not a control, format or separator.

    Why: the broker refuses a key the worker would refuse, before sending it.
    What: mirrors ``keyAllowed`` in extension/pages.js.
    Test: `test_page_press_validates_key_and_modifiers_before_sending`.
    """
    if not isinstance(key, str):
        return False
    if key in NAMED_KEYS:
        return True
    return len(key) == 1 and unicodedata.category(key)[0] not in "CZ"
