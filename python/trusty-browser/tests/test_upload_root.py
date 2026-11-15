"""``page_upload`` reads only regular files under ``--upload-root`` (PR #11305 review).

A hostile page can ask the agent to "attach" ~/.ssh/id_rsa; each case below
is a path that must never be read.
"""

import json
import os
from pathlib import Path

import pytest

from trusty_browser import agent_mcp


@pytest.fixture()
def tree(tmp_path):
    root = tmp_path / "root"
    (root / "sub").mkdir(parents=True)
    (root / "ok.txt").write_text("ok")
    (root / "sub" / "deep.txt").write_text("deep")
    secret = tmp_path / "secret"
    secret.mkdir()
    (secret / "id_rsa").write_text("PRIVATE KEY")
    return agent_mcp.upload_root(str(root)), secret / "id_rsa"


def refusal(raw, root):
    with pytest.raises(agent_mcp.Refused) as info:
        agent_mcp.upload_file(raw, root)
    return json.loads(str(info.value))


def reason(raw, root):
    frame = refusal(raw, root)
    assert frame["error"] == "FileRefused", frame
    return frame["message"].removeprefix(f"{raw}: ")


def test_files_inside_the_root_are_read(tree):
    root, _ = tree
    assert agent_mcp.upload_file("ok.txt", root)["data"] == "b2s="
    deep = agent_mcp.upload_file(str(root / "sub" / "deep.txt"), root)
    assert deep["name"] == "deep.txt" and deep["data"] == "ZGVlcA=="


def test_no_root_refuses_every_upload(tree):
    root, _ = tree
    frame = refusal(str(root / "ok.txt"), None)
    assert frame["error"] == "UploadsDisabled"


def test_symlink_escape_is_refused(tree):
    root, secret = tree
    (root / "leak").symlink_to(secret)
    (root / "dirlink").symlink_to(secret.parent, target_is_directory=True)
    assert reason("leak", root) == "is a symlink"
    assert reason(str(root / "leak"), root) == "is a symlink"
    # A symlinked directory is refused too, though its target is a directory.
    assert reason("dirlink/id_rsa", root).startswith("is a symlink")
    assert (
        reason(str(root / "dirlink" / "id_rsa"), root) == "is outside the upload root"
    )


def test_symlink_inside_the_root_is_refused(tree):
    # Not an escape, but a link can be repointed between check and use.
    root, _ = tree
    (root / "alias").symlink_to(root / "ok.txt")
    assert reason("alias", root) == "is a symlink"


@pytest.mark.parametrize(
    "raw", ["../secret/id_rsa", "sub/../../secret/id_rsa", "sub/../ok.txt"]
)
def test_dot_dot_components_are_refused(tree, raw):
    root, _ = tree
    assert reason(raw, root) == "contains a '..' component"
    assert reason(f"{root}/{raw}", root) == "contains a '..' component"


def test_absolute_path_outside_the_root_is_refused(tree):
    root, secret = tree
    assert reason(str(secret), root) == "is outside the upload root"
    assert reason("/etc/hosts", root) == "is outside the upload root"
    # A sibling sharing the root's name as a prefix is still outside.
    sibling = root.parent / (root.name + "-x")
    sibling.mkdir()
    (sibling / "f").write_text("x")
    assert reason(str(sibling / "f"), root) == "is outside the upload root"


def test_fifo_and_device_are_refused(tree):
    root, _ = tree
    os.mkfifo(root / "pipe")
    assert reason("pipe", root) == "is not a regular file"  # And does not hang.
    assert reason("sub", root) == "is not a regular file"
    dev = Path("/dev/null")
    assert reason(str(dev), agent_mcp.upload_root("/dev")) == "is not a regular file"


@pytest.mark.skipif(os.geteuid() == 0, reason="root reads mode-000 files")
def test_unreadable_file_is_refused(tree):
    root, _ = tree
    locked = root / "locked.txt"
    locked.write_text("x")
    locked.chmod(0)
    try:
        assert reason("locked.txt", root) == "cannot be read"
    finally:
        locked.chmod(0o600)


def test_upload_root_must_be_a_directory(tree):
    root, _ = tree
    with pytest.raises(ValueError):
        agent_mcp.upload_root(str(root / "ok.txt"))
