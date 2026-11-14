import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys

import pytest

from trusty_browser import cli
from trusty_browser.assets import extension_path


def test_stable_assets_are_complete_and_refuse_overwriting(tmp_path):
    state = tmp_path/"state"
    path = extension_path(state)
    assert (path/"manifest.json").is_file()
    assert path == extension_path(state)
    path.unlink()
    path.write_text("other")
    with pytest.raises(ValueError,match="unrelated"):
        extension_path(state)
    assert path.read_text() == "other"


def test_service_registration_install_repeat_uninstall_in_private_home(tmp_path,monkeypatch):
    state=tmp_path/"state"
    monkeypatch.setenv("HOME",str(tmp_path))
    monkeypatch.setattr(sys,"platform","darwin")
    plist=tmp_path/"Library/LaunchAgents/com.trusty.browser.plist"
    manifests=tmp_path/"Library/Application Support/Google/Chrome/NativeMessagingHosts"
    loaded=False
    def launchctl(*args,check=True):
        nonlocal loaded
        if args[0]=="print":
            return subprocess.CompletedProcess(args,0 if loaded else 113,stdout=f"path = {plist}\n",stderr="")
        if args[0]=="bootstrap":
            assert not loaded and args[-1] == str(plist)
            loaded=True
        if args[0]=="bootout":
            assert loaded
            loaded=False
    monkeypatch.setattr(cli,"launchctl",launchctl)
    cli.manage(state,True)
    assert loaded
    assert plistlib.loads(plist.read_bytes())["ProgramArguments"] == cli.service_args(state)
    for name in ("com.trusty.browser","com.trusty.browser.enroll"):
        assert Path(json.loads((manifests/(name+".json")).read_text())["path"]).is_file()
    cli.manage(state,True)
    cli.manage(state,False)
    cli.manage(state,False)
    assert not loaded and not plist.exists() and not list(manifests.glob("*.json"))
    assert (state/"extension"/"manifest.json").exists()
