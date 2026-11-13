"""Round-trip authenticated wrapping through a fresh macOS Keychain item."""
import os

import pytest
from keyring.backends.macOS import Keyring

from trusty_browser import vault


def test_secret_is_bound_to_its_purpose_and_keychain(tmp_path):
    os.chmod(tmp_path, 0o700)
    keyring = Keyring()
    try:
        sealed = vault.seal(tmp_path, "ticket:travel", "clear-test-grant")
        assert "clear-test-grant" not in sealed
        assert vault.unseal(tmp_path, "ticket:travel", sealed) == "clear-test-grant"
        with pytest.raises(ValueError):
            vault.unseal(tmp_path, "ticket:personal", sealed)
        with pytest.raises(ValueError):
            vault.unseal(tmp_path, "ticket:travel", "clear-test-grant")
    finally:
        keyring.delete_password(vault.SERVICE, str(tmp_path.resolve()))
    with pytest.raises(ValueError):
        vault.unseal(tmp_path, "ticket:travel", sealed)
