"""PR-B2: fleet_poll_deps.py raises one urgent `dep_degraded` per memory/search outage.

Why: the owner ruled the poller alerts when tm health is degraded because trusty-memory or trusty-search is
down, on 2 consecutive degraded reads, never on pending.
What: drives dep_health_alerts with injected tm readings (and one real Unix socket for the wire read), the
fleet-poll.py wiring, the triage class, and the mirror/provenance pairing. Every test is new behaviour: the
module, the kind and the wiring do not exist at the base commit.
"""
import hashlib
import importlib.util
import json
import os
import socket
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
SUB = os.path.dirname(HERE)
SCRIPTS = os.path.join(SUB, "scripts")
SHIPPED = os.path.join(os.path.dirname(os.path.dirname(SUB)),
                       "crates", "trusty-mpm", "src", "assets", "architect", "scripts")
ON = {"TM_HEALTH": "on"}


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


deps = _load("fleet_poll_deps_t", "fleet_poll_deps.py")
classify_mod = _load("fleet_classify_deps_t", "fleet-classify.py")
alert = classify_mod.alert

DOWN = {"status": "degraded", "degraded": ["trusty-memory: down — refused"], "pending": []}
UP = {"status": "ok", "degraded": [], "pending": []}
PENDING = {"status": "ok", "degraded": [], "pending": ["trusty-memory: pending"]}


def run(state, result, now=1.0):
    found, _ = deps.dep_health_alerts(state, now, alert, read=lambda: ("ok", result), env=ON)
    return found


def test_one_degraded_read_raises_nothing():
    assert run({}, DOWN) == []


def test_two_consecutive_raise_exactly_one():
    state = {}
    run(state, DOWN)
    found = run(state, DOWN)
    assert [(a["kind"], a["fingerprint"], a["session"]) for a in found] == [("dep_degraded", "trusty-memory", "")]
    assert "refused" in found[0]["detail"]


def test_third_read_does_not_duplicate():
    state = {}
    run(state, DOWN), run(state, DOWN)
    assert run(state, DOWN) == []


def test_recovery_then_two_degraded_alerts_again():
    state = {}
    run(state, DOWN), run(state, DOWN)
    assert run(state, UP) == []
    assert state["dep_health"] == {}
    assert run(state, DOWN) == []
    assert len(run(state, DOWN)) == 1


def test_pending_read_does_not_count():
    state = {}
    assert run(state, PENDING) == [] and run(state, PENDING) == []
    assert run(state, DOWN) == []                     # pending counted nothing
    assert run(state, PENDING) == []                  # and reset nothing
    assert len(run(state, DOWN)) == 1
    # A pre-PR-B2 daemon listed pending inside degraded[]; it is still pending.
    assert deps.classify({"degraded": ["trusty-search: pending (first probe still running)"]})[
        "trusty-search"] == "pending"


def test_each_dependency_dedupes_on_its_own():
    state = {}
    both = {"degraded": ["trusty-memory: timeout", "trusty-search: down — x"], "pending": []}
    run(state, both)
    assert sorted(a["fingerprint"] for a in run(state, both)) == ["trusty-memory", "trusty-search"]
    assert run(state, both) == []


def test_garbage_reply_is_the_error_arm():
    state = {"dep_health": {"trusty-memory": {"streak": 1, "alerted": False}}}
    for reading in (("ok", "not a dict"), ("ok", {"degraded": "x"}), ("error", ("unparseable", "junk"))):
        found, _ = deps.dep_health_alerts(state, 1.0, alert, read=lambda r=reading: r, env=ON)
        assert [a["kind"] for a in found] == ["dep_health_error"], reading
    def boom():
        raise RuntimeError("bad")
    found, _ = deps.dep_health_alerts(state, 1.0, alert, read=boom, env=ON)
    assert found[0]["fingerprint"] == "RuntimeError"
    assert state["dep_health"]["trusty-memory"]["streak"] == 1, "no false healthy: streak held"


def _serve_once(path, reply):
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(path)
    srv.listen(1)

    def handle():
        conn, _ = srv.accept()
        conn.recv(65536)
        conn.sendall(reply)
        conn.close()
        srv.close()
    threading.Thread(target=handle, daemon=True).start()


def _short_dir():
    import tempfile
    return tempfile.mkdtemp(prefix="pd", dir="/tmp")  # sun_path is short


def test_wire_read_parses_a_real_socket_and_garbage():
    d = _short_dir()
    ok = os.path.join(d, "a.sock")
    _serve_once(ok, (json.dumps({"jsonrpc": "2.0", "id": 1, "result": DOWN}) + "\n").encode())
    assert deps.read_tm_health(ok) == ("ok", DOWN)
    bad = os.path.join(d, "b.sock")
    _serve_once(bad, b"<html>nope\n")
    status, (cls, _) = deps.read_tm_health(bad)
    assert (status, cls) == ("error", "unparseable")


def test_unreachable_socket_is_deferred_not_healthy(tmp_path):
    state = {"dep_health": {"trusty-memory": {"streak": 1, "alerted": False}}}
    env = {"TM_HEALTH_SOCKET": str(tmp_path / "absent.sock")}
    read = lambda: deps.read_tm_health(env["TM_HEALTH_SOCKET"])  # noqa: E731
    found, lines = deps.dep_health_alerts(state, 1.0, alert, read=read, env=env)
    assert found == [] and "unreachable" in lines[0]
    assert state["dep_health"]["trusty-memory"]["streak"] == 1


def test_off_switch_and_socket_path():
    assert deps.dep_health_alerts({}, 1.0, alert, read=lambda: ("ok", DOWN), env={"TM_HEALTH": "off"}) == ([], [])
    assert deps.socket_path({"TRUSTY_DATA_DIR_OVERRIDE": "/x"}) == "/x/trusty-mpm/trusty-mpm.sock"
    assert deps.socket_path({"TM_HEALTH_SOCKET": "/s"}) == "/s"


def test_triage_marks_dep_degraded_urgent_and_waking():
    at = _load("alert_triage_deps_t", "alert_triage.py")
    a = alert("dep_degraded", "trusty-memory", "d")
    assert at.is_urgent(a)
    assert at.decide(a, {"holds": []}, 1.0, {})["decision"] == "wake"


def test_fleet_poll_binds_the_step_into_the_cycle():
    fp = _load("fleet_poll_deps_wiring_t", "fleet-poll.py")
    assert fp.fleet_poll_deps.dep_health_alerts.__doc__
    src = open(os.path.join(SCRIPTS, "fleet-poll.py")).read()
    cycle_src = src[src.index("def cycle("):src.index("def load_state(")]
    assert "fleet_poll_deps.dep_health_alerts(state, now, alert)" in cycle_src
    assert "candidates += dep_found" in cycle_src


def test_mirror_and_provenance_stay_in_sync():
    mine = open(os.path.join(SCRIPTS, "fleet_poll_deps.py"), "rb").read()
    if os.path.isdir(SHIPPED):
        assert open(os.path.join(SHIPPED, "fleet_poll_deps.py"), "rb").read() == mine
    prov = json.load(open(os.path.join(SUB, "provenance.json")))
    assert prov["files"]["scripts/fleet_poll_deps.py"] == hashlib.sha256(mine).hexdigest()
