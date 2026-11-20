"""Read a Rust-written state dir with the Python 0.7.0 package (reverse parity).

Usage: python -I reverse_check.py <state-dir> <key-b64>
The vault's Keychain is replaced by a stub that answers only the production
service name for the resolved state path; no Keychain is touched. Prints the
unsealed pairings and the audit line count as JSON.
"""

import json
import pathlib
import sys

state = pathlib.Path(sys.argv[1])
key = sys.argv[2]

import keyring.backends.macOS as mac  # noqa: E402


class Stub:
    def get_password(self, service: str, account: str) -> str:
        assert service == "com.trusty.browser.local-credentials", service
        assert account == str(state.resolve()), account
        return key

    def set_password(self, *args: object) -> None:
        raise AssertionError("reverse check must not create a key")


mac.Keyring = Stub

from trusty_browser import enrollment, pairing  # noqa: E402

db = enrollment.database(state)
out = []
for item in pairing.pairings(db):
    row = pairing.profile_row(db, item["profile"])
    out.append({"profile": item["profile"], "email": item["email"],
                "key": enrollment.browser_key(state, row)})
lines = (state / "audit.jsonl").read_bytes().splitlines(keepends=True)
for line in lines:
    again = (json.dumps(json.loads(line), separators=(",", ":"), allow_nan=False) + "\n").encode()
    assert again == line, line
print(json.dumps({"pairings": out, "audit_lines": len(lines)}, ensure_ascii=False))
