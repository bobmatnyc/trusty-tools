"""rc-lib.sh against a fake tmux: rc_connect (fresh, disconnect, stuck dialog) and rc_send_cmd echo checks."""
import json
import os
import stat
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
LIB = os.path.join(HERE, "rc-lib.sh")

FAKE_TMUX = r'''#!/usr/bin/env python3
import json, sys
SF = __STATE__
st = json.load(open(SF))
a = sys.argv[1:]
if a[:1] == ["-S"]:
    a = a[2:]
cmd = a[0]
R = "-" * 20
def save(): json.dump(st, open(SF, "w"))
if cmd == "capture-pane":
    out = ["https://claude.ai/code/session_OLD1" if st["active"] else ""]
    if st["mode"] == "dialog":
        out += ["Remote Control", ("❯ " if st["sel"] == 0 else "  ") + "Disconnect this session",
                ("❯ " if st["sel"] == 1 else "  ") + "Show QR code",
                ("❯ " if st["sel"] == 2 else "  ") + "Continue"]
    else:
        if st["msg"]:
            out.append(st["msg"])
        if st["url"]:
            out.append(st["url"])
        out += [R, "❯ " + st["input"] + st.get("menu", ""), R]
    print("\n".join(out))
elif cmd == "send-keys":
    keys = a[3:]
    if keys[0] == "-l":
        st["input"] += keys[1]
    elif keys[0] == "C-u":
        st["input"] = ""; st["cu"] = st.get("cu", 0) + 1
    elif keys[0] == "Up":
        if not st["stuck"]:
            st["sel"] = max(0, st["sel"] - 1)
    elif keys[0] == "Escape":
        st["mode"] = "prompt"; st["esc"] = True
    elif keys[0] == "Enter":
        st["enters"] = st.get("enters", 0) + 1
        if st["mode"] == "dialog":
            if st["sel"] == 0:
                st["active"] = False; st["msg"] = "Remote Control disconnected."
            st["mode"] = "prompt"
        elif st["input"].startswith("/remote-control "):
            st["cmds"].append(st["input"]); st["input"] = ""; st["msg"] = ""
            if st["active"]:
                st["mode"] = "dialog"; st["sel"] = 2
            else:
                st["active"] = True; st["url"] = "https://claude.ai/code/session_NEW9"
        else:
            st["input"] = ""
    save()
'''


def _run(tmp_path, active, stuck=False, call="rc_connect", **extra):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    state = tmp_path / "state.json"
    state.write_text(json.dumps({**dict(active=active, mode="prompt", sel=2, input="", msg="",
                                        url="", cmds=[], stuck=stuck, esc=False), **extra}))
    for name, body in (("tmux", FAKE_TMUX.replace("__STATE__", repr(str(state)))),
                       ("sleep", "#!/bin/sh\nexit 0\n")):
        p = bindir / name
        p.write_text(body)
        p.chmod(p.stat().st_mode | stat.S_IEXEC)
    script = f'. "{LIB}"; s=tm-x; t="=tm-x:"; log() {{ :; }}; RC_ECHO_TRIES=2; {call}; echo "rc=$? url=$RC_URL fail=$RC_FAIL"'
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}")
    out = subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True, timeout=60)
    return out.stdout.strip(), json.loads(state.read_text())


def test_fresh_connect_names_session(tmp_path):
    out, st = _run(tmp_path, active=False)
    assert out == "rc=0 url=https://claude.ai/code/session_NEW9 fail="
    assert st["cmds"] == ["/remote-control tm-x"]


def test_active_rc_disconnects_then_retypes(tmp_path):
    out, st = _run(tmp_path, active=True)
    assert out == "rc=0 url=https://claude.ai/code/session_NEW9 fail="
    assert st["cmds"] == ["/remote-control tm-x", "/remote-control tm-x"]


def test_stuck_dialog_escapes_and_fails(tmp_path):
    out, st = _run(tmp_path, active=True, stuck=True)
    assert out.startswith("rc=1 url= fail=RC dialog: Disconnect not reached")
    assert st["esc"] is True


MENU = "              Resume from a paused PM session - scan ..."


def test_send_cmd_accepts_slash_menu_description(tmp_path):
    out, st = _run(tmp_path, active=False, call='rc_send_cmd "/tm-session-resume"', menu=MENU)
    assert out.startswith("rc=0")
    assert st["enters"] == 1


def test_send_cmd_never_submits_a_user_draft(tmp_path):
    out, st = _run(tmp_path, active=False, call='rc_send_cmd "/rename tm-x"', input="my draft")
    assert out.startswith("rc=1")
    assert st.get("enters", 0) == 0 and st.get("cu", 0) == 0
    assert st["input"] == "my draft/rename tm-x"


def test_send_cmd_clears_only_its_own_duplicate(tmp_path):
    out, st = _run(tmp_path, active=False, call='rc_send_cmd "/rename tm-x"', input="/rename tm-x")
    assert out.startswith("rc=1")
    assert st.get("enters", 0) == 0 and st["cu"] == 1 and st["input"] == ""
