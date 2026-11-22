"""fleet_poll_deps.py — the poller's `dep_degraded` alert: tm health degraded because memory or search is down (PR-B2).

Why: the owner ruled that the poller alerts, urgently, when tm health is degraded because trusty-memory or
trusty-search is down. One degraded read can be a blip, and tm reports "pending" while a probe is still in flight,
so the rule needs two consecutive degraded reads and must ignore pending.
What: `read_tm_health` asks tm's `mpm.health` over its Unix socket (newline-framed JSON-RPC, the protocol of
trusty_common::uds::rpc). `classify` maps the answer to up / pending / degraded per service. `step` is the pure rule:
a service degraded on 2 consecutive reads raises ONE `alert("dep_degraded", <service>, ...)` per outage; pending
neither counts nor resets the streak; an up read clears the service, so the next outage alerts again.
`dep_health_alerts(state, now, alert)` is the entry point a poller calls once per cycle; its state lives under
state["dep_health"]. A garbage answer is a `dep_health_error` alert whose fingerprint is the error class (the
owner_answer_error precedent); an unreachable tm is only logged, because tm being unreachable is its own alert,
not this one. Neither ever reads as healthy: the streaks are kept as they were.
Self-contained (stdlib only, no poller binding) so it can be dropped beside any fleet poller.
Config (env): TM_HEALTH=off turns the step off; TM_HEALTH_SOCKET names the socket, else
<data dir>/trusty-mpm/trusty-mpm.sock, where the data dir follows TRUSTY_DATA_DIR_OVERRIDE as tm does.
Test: tests/test_fleet_poll_deps.py
"""
import json
import os
import socket
import sys

KIND = "dep_degraded"
ERROR_KIND = "dep_health_error"
SERVICES = ("trusty-memory", "trusty-search")
CONSECUTIVE = 2          # degraded reads in a row before one alert
STATE_KEY = "dep_health"
TIMEOUT_S = 2.0
MAX_FRAME = 1 << 20


def socket_path(env=None):
    """tm's socket: TM_HEALTH_SOCKET, else tm's own derivation (trusty_common::daemon_socket_path)."""
    env = os.environ if env is None else env
    if env.get("TM_HEALTH_SOCKET"):
        return env["TM_HEALTH_SOCKET"]
    home = os.path.expanduser("~")
    base = env.get("TRUSTY_DATA_DIR_OVERRIDE", "").strip()
    if not (base and os.path.isabs(base) and base != "/"):
        if sys.platform == "darwin":
            base = os.path.join(home, "Library", "Application Support")
        else:
            base = env.get("XDG_DATA_HOME") or os.path.join(home, ".local", "share")
    if not base.endswith(".trusty-mpm"):
        base = os.path.join(base, "trusty-mpm")
    return os.path.join(base, "trusty-mpm.sock")


def read_tm_health(path=None, timeout=TIMEOUT_S):
    """One `mpm.health` call: ("ok", result) | ("unreachable", detail) | ("error", (class, detail)).

    Why: the ruling reads tm health over its UDS method, not HTTP /health.
    What: connect, write one newline-terminated JSON-RPC frame, half-close, read one line. A missing socket or a
    refused connect is "unreachable"; a timeout, an over-long or non-JSON reply, or an RPC error is "error".
    Test: test_unreachable_socket_is_deferred_not_healthy, test_garbage_reply_is_the_error_arm
    """
    path = path or socket_path()
    if not os.path.exists(path):
        return "unreachable", f"{path}: no socket"
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        try:
            s.connect(path)
        except (FileNotFoundError, ConnectionRefusedError) as e:
            return "unreachable", f"{path}: {e.strerror or e}"
        req = {"jsonrpc": "2.0", "id": 1, "method": "mpm.health", "params": {}}
        s.sendall((json.dumps(req) + "\n").encode())
        s.shutdown(socket.SHUT_WR)
        buf = b""
        while b"\n" not in buf and len(buf) <= MAX_FRAME:
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
    except socket.timeout:
        return "error", ("timeout", f"no answer within {timeout}s")
    except OSError as e:
        return "error", ("io", str(e))
    finally:
        s.close()
    try:
        reply = json.loads(buf.split(b"\n", 1)[0].decode())
    except (UnicodeDecodeError, ValueError):
        return "error", ("unparseable", f"reply is not JSON ({len(buf)} bytes)")
    if not isinstance(reply, dict) or "result" not in reply:
        err = reply.get("error") if isinstance(reply, dict) else None
        return "error", ("rpc_error", json.dumps(err)[:200] if err else "reply has no result")
    return "ok", reply["result"]


def classify(result):
    """{service: "up" | "pending" | "degraded"} for an mpm.health result; ValueError on a bad shape.

    What: a service is degraded when degraded[] has a "<service>: ..." entry other than a pending one, pending when
    pending[] (or a pre-PR-B2 "<service>: pending" degraded entry) names it, else up.
    Test: test_pending_read_does_not_count, test_garbage_reply_is_the_error_arm
    """
    if not isinstance(result, dict):
        raise ValueError("result is not an object")
    degraded, pending = result.get("degraded", []), result.get("pending", [])
    if not isinstance(degraded, list) or not isinstance(pending, list):
        raise ValueError("degraded/pending is not a list")
    out = {}
    for svc in SERVICES:
        mine = [str(e) for e in degraded if str(e).startswith(svc + ":")]
        if any(not e[len(svc) + 1:].strip().startswith("pending") for e in mine):
            out[svc] = "degraded"
        elif mine or any(str(e).startswith(svc + ":") for e in pending):
            out[svc] = "pending"
        else:
            out[svc] = "up"
    return out


def step(book, reading, result, now, alert):
    """The pure rule for one classified read; returns new alerts and updates `book` in place.

    `book` is {service: {"streak": n, "alerted": bool}}; `reading` is classify()'s output; `result` the raw answer.
    Test: test_one_degraded_read_raises_nothing, test_two_consecutive_raise_exactly_one,
    test_third_read_does_not_duplicate, test_recovery_then_two_degraded_alerts_again, test_pending_read_does_not_count
    """
    found = []
    for svc in SERVICES:
        state = reading.get(svc, "up")
        if state == "up":
            book.pop(svc, None)
            continue
        if state == "pending":
            continue  # neither counts nor resets
        entry = book.setdefault(svc, {"streak": 0, "alerted": False})
        entry["streak"] += 1
        if entry["streak"] >= CONSECUTIVE and not entry["alerted"]:
            entry["alerted"] = True
            why = "; ".join(str(e) for e in result.get("degraded", []) if str(e).startswith(svc + ":"))
            found.append(alert(KIND, svc, f"tm health degraded: {why or svc}", now=now))
    return found


def dep_health_alerts(state, now, alert, read=None, env=None):
    """The poller step: (alerts, log lines) for this cycle; dedupe state in state["dep_health"].

    Why/What: see the module doc. `alert` is the poller's alert(kind, fingerprint, detail, session, pane, now);
    `read` defaults to read_tm_health. Never raises: a bad answer is the error arm, an unreachable tm a log line.
    Test: tests/test_fleet_poll_deps.py
    """
    env = os.environ if env is None else env
    if env.get("TM_HEALTH") == "off":
        return [], []
    book = state.setdefault(STATE_KEY, {})
    try:
        status, payload = (read or read_tm_health)()
        if status == "unreachable":
            return [], [f"tm health unreachable ({payload}); dep_degraded deferred, streaks held"]
        if status != "ok":
            cls, detail = payload
            return [alert(ERROR_KIND, cls, f"cannot read tm health ({cls}: {detail}); streaks held",
                          now=now)], []
        reading = classify(payload)
    except Exception as e:  # noqa: BLE001 - a bad answer must not stop the poll loop
        cls = type(e).__name__
        return [alert(ERROR_KIND, cls, f"cannot read tm health ({cls}: {e}); streaks held", now=now)], []
    return step(book, reading, payload, now, alert), []
