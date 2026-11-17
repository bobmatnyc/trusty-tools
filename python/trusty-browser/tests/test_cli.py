# ruff: noqa: F811 -- the imported fixtures `short` and `clock` are test parameters.

import json
import shutil
import plistlib
import subprocess
import sys
from pathlib import Path

import pytest

from trusty_browser import cli
from trusty_browser.assets import extension_path


def test_stable_assets_are_complete_and_refuse_overwriting(tmp_path):
    state = tmp_path / "state"
    path = extension_path(state)
    assert (path / "manifest.json").is_file()
    assert path == extension_path(state)
    shutil.rmtree(path)
    path.write_text("other")
    with pytest.raises(ValueError, match="unrelated"):
        extension_path(state)
    assert path.read_text() == "other"


def test_service_registration_install_repeat_uninstall_in_private_home(
    tmp_path, monkeypatch
):
    state = tmp_path / "state"
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(sys, "platform", "darwin")
    plist = tmp_path / "Library/LaunchAgents/com.trusty.browser.plist"
    manifests = (
        tmp_path / "Library/Application Support/Google/Chrome/NativeMessagingHosts"
    )
    loaded = False

    def launchctl(*args, check=True):
        nonlocal loaded
        if args[0] == "print":
            return subprocess.CompletedProcess(
                args, 0 if loaded else 113, stdout=f"path = {plist}\n", stderr=""
            )
        if args[0] == "bootstrap":
            assert not loaded and args[-1] == str(plist)
            loaded = True
        if args[0] == "bootout":
            assert loaded
            loaded = False

    monkeypatch.setattr(cli, "launchctl", launchctl)
    cli.manage(state, True)
    assert loaded
    assert plistlib.loads(plist.read_bytes())["ProgramArguments"] == cli.service_args(
        state
    )
    for name in ("com.trusty.browser", "com.trusty.browser.enroll"):
        assert Path(
            json.loads((manifests / (name + ".json")).read_text())["path"]
        ).is_file()
    cli.manage(state, True)
    cli.manage(state, False)
    cli.manage(state, False)
    assert not loaded and not plist.exists() and not list(manifests.glob("*.json"))
    assert (state / "extension" / "manifest.json").exists()


# #11239: operator verbs against the real broker; fixtures from the code tests.
from test_activation_codes import clock, run, short  # noqa: F401


def test_operator_verbs_validate_input_and_print_the_reply(
    short, clock, monkeypatch, capsys
):
    import asyncio

    def verb(*argv):
        monkeypatch.setattr(
            sys, "argv", ["trusty-browser", "--state", str(short), *argv]
        )
        try:
            cli.main()
        except SystemExit as exc:
            return exc.code
        return 0

    async def body(rig):
        minted = await rig.grant(rig.alpha)
        assert await asyncio.to_thread(verb, "grants") == 0
        listed = json.loads(capsys.readouterr().out)
        assert [g["grant"] for g in listed["grants"]] == [minted["grant"]]
        assert rig.alpha["key"] not in json.dumps(listed)
        for argv in (
            ("revoke-grant", "G-XYZ"),
            ("revoke-grant", minted["grant"] + "0"),
            ("unpair", "not-a-uuid"),
        ):
            assert await asyncio.to_thread(verb, *argv) == 1
            assert capsys.readouterr().out == ""
        assert rig.outcomes("revoke") == []
        assert await asyncio.to_thread(verb, "revoke-grant", "g-000000000000") == 1
        assert json.loads(capsys.readouterr().out)["error"] == "UnknownGrant"
        assert await asyncio.to_thread(verb, "revoke-grant", minted["grant"]) == 0
        assert json.loads(capsys.readouterr().out) == {"ok": True, "revoked": 1}
        assert await asyncio.to_thread(verb, "pairings") == 0
        assert json.loads(capsys.readouterr().out) == {"ok": True, "pairings": []}
        profile = "3f2b6c1e-8d4a-4b7e-9c1d-2a5e6f7b8c9d"
        assert await asyncio.to_thread(verb, "unpair", profile) == 1
        assert json.loads(capsys.readouterr().out)["error"] == "UnknownProfile"
        assert await asyncio.to_thread(verb, "revoke-grants") == 0
        assert json.loads(capsys.readouterr().out) == {"ok": True, "revoked": 0}

    run(short, body)
    assert verb("grants") == 1  # the broker has stopped
    assert "not reachable" in capsys.readouterr().err


def test_republish_reaches_a_profile_holding_an_old_hash_path(tmp_path):
    # #11363: Chrome keeps the resolved path of the old symlinked layout.
    from trusty_browser import assets

    state = tmp_path / "state"
    old = state / "assets" / ("a" * 64)
    state.mkdir(mode=0o700)
    (state / "assets").mkdir(mode=0o700)
    old.mkdir()
    (old / "manifest.json").write_text('{"version": "0.1.0"}')
    (old / "setup.html").write_text("old enroll popup")
    (state / "extension").symlink_to(Path("assets") / ("a" * 64))
    source = tmp_path / "source"
    shutil.copytree(Path(assets.__file__).with_name("extension"), source)

    path = extension_path(state, source)
    assert not path.is_symlink() and path.is_dir()
    assert old.is_symlink() and old.resolve() == path.resolve()
    assert (old / "setup.html").read_text() == (source / "setup.html").read_text()
    first = assets.published(state)
    assert first == assets.stamp(source)

    (source / "setup.html").write_text("republished popup")
    (source / "gone.js").write_text("x")
    extension_path(state, source)
    (source / "gone.js").unlink()
    extension_path(state, source)
    # The profile's stored path, the old hash dir, serves the current assets.
    assert (old / "setup.html").read_text() == "republished popup"
    assert not (old / "gone.js").exists()
    assert assets.published(state) == assets.stamp(source) != first
    manifest = json.loads((old / "manifest.json").read_text())
    assert manifest["version_name"].endswith("+" + assets.stamp(source))
