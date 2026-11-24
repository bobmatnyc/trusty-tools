#!/usr/bin/env python3
"""user_state.py: the owner's presence state, set from the dashboard settings pane.

Why: the owner (2026-10-09) wants engagement and token use tuned to whether the owner is at the computer, on a
device, away or asleep. The dashboard writes meta/settings; the poller and the pending-answers hook read it.
What: read(root) returns {state, at, by} from meta/settings through dashboard_store. State is one of STATES;
unset or unknown values give DEFAULT ("active") with at "".
Test: tests/test_user_state.py
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402

STATES = ("active", "remote", "mobile", "away", "sleeping")
DEFAULT = "active"


def read(root=None):
    try:
        data = ds.Store(root or ds.DEFAULT_ROOT).get("meta", "settings")["data"]
    except ds.NotFound:
        return {"state": DEFAULT, "at": "", "by": ""}
    if data.get("user_state") not in STATES:
        return {"state": DEFAULT, "at": "", "by": ""}
    return {"state": data["user_state"], "at": str(data.get("user_state_at", "")), "by": str(data.get("user_state_by", ""))}


# ---- effective state (owner, 2026-10-29: "If I respond to questions within 15 minutes, I'm not sleeping.") ----
import time  # noqa: E402
from datetime import datetime, timezone  # noqa: E402

WINDOW_SECS = 900
SIGNAL_THROTTLE_SECS = 60
SIGNAL_WRITERS_IGNORED = "architect"  # an Architect-written stamp is never an owner signal
AUTO_PREFIX = "auto:"  # actor of an automatic sleeping/away -> active switch; not itself an owner signal
AUTO_FROM = ("sleeping", "away")  # only these flip; active and remote stay, and nothing auto-switches to sleeping or away
# owner 2026-10-30 ~03:40Z: "Don't count that as me being awake, I'm setting it to walk away." A manual set to sleeping or
# away is not a signal, and signals up to GRACE_SECS after it (the turn that sets it) are ignored.
GRACE_SECS = 300


def _hold_until(data):
    """Epoch until which signals are ignored: manual set time + GRACE_SECS when the owner set sleeping/away, else 0.0.
    Architect-written and auto: stamps are not manual sets."""
    by = str(data.get("user_state_by", "")).lower()
    if data.get("user_state") not in AUTO_FROM or SIGNAL_WRITERS_IGNORED in by or by.startswith(AUTO_PREFIX):
        return 0.0
    at = _epoch(data.get("user_state_at"))
    return at + GRACE_SECS if at else 0.0


def _signal_patch(data, kind, sig_epoch, now):
    """Fields for meta/settings: the signal stamp, plus user_state=active when the stored state is sleeping or away and the
    signal is under WINDOW_SECS old (owner, 2026-10-29: the dropdown must not say Sleeping while the owner is chatting)."""
    patch = {"owner_signal_at": _iso(sig_epoch), "owner_signal_kind": kind}
    if data.get("user_state") in AUTO_FROM and 0 <= now - sig_epoch < WINDOW_SECS and sig_epoch > _hold_until(data):
        patch.update({"user_state": "active", "user_state_at": _iso(now), "user_state_by": f"{AUTO_PREFIX} owner activity ({kind})"})
    return patch


def _write_signal(store, doc, kind, sig_epoch, now):
    patch = _signal_patch(doc["data"], kind, sig_epoch, now)
    actor = patch.get("user_state_by")  # the change-log actor names the auto switch
    try:
        store.update("meta", "settings", patch, if_version=doc["version"], actor=actor)
    except ds.Conflict:
        return False  # a concurrent owner edit wins
    return True


def _epoch(iso):
    try:
        return datetime.fromisoformat(str(iso).replace("Z", "+00:00")).astimezone(timezone.utc).timestamp()
    except (ValueError, TypeError):
        return 0.0


def _iso(epoch):
    return datetime.fromtimestamp(epoch, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z")


def _walk(store):
    """Yield (epoch, label) for each owner write found in the store: answers, status requests, parks, session commands,
    effort tunes and steers. A doc whose answered_by names the Architect is skipped."""
    for coll in ("items", "sessions", "il_steers", "effort_tunes"):
        try:
            docs = store.list_docs(coll)
        except Exception:  # noqa: BLE001 - one unreadable collection must not hide the others
            continue
        for d in docs:
            data, did = d["data"], d["id"]
            if SIGNAL_WRITERS_IGNORED in str(data.get("answered_by", "")).lower():
                continue
            if coll == "items":
                yield _epoch(data.get("answered_at")), f"answered {did}"
                for key, what in (("status_request", "asked status on"), ("parked", "parked")):
                    sub = data.get(key)
                    if isinstance(sub, dict):
                        yield _epoch(sub.get("at")), f"{what} {did}"
            elif coll == "sessions":
                sub = data.get("command")
                if isinstance(sub, dict):
                    yield _epoch(sub.get("at")), f"sent a command to {did}"
            else:
                yield _epoch(data.get("at")), f"{coll[:-1] if coll.endswith('s') else coll} {did}"


def last_signal(root=None):
    """Newest owner signal as (epoch, label), or (0.0, "") when none exists."""
    store = ds.Store(root or ds.DEFAULT_ROOT)
    best = (0.0, "")
    try:
        s = store.get("meta", "settings")["data"]
    except ds.NotFound:
        s = {}
    if s.get("owner_signal_at"):
        best = (_epoch(s["owner_signal_at"]), str(s.get("owner_signal_kind") or "owner turn"))
    by = str(s.get("user_state_by", "")).lower()
    if s.get("user_state_at") and SIGNAL_WRITERS_IGNORED not in by and not by.startswith(AUTO_PREFIX) and s.get("user_state") not in AUTO_FROM:
        best = max(best, (_epoch(s["user_state_at"]), "set the state in the dashboard"))
    hold = _hold_until(s)
    return max([(0.0, "")] + [c for c in [best] + list(_walk(store)) if c[0] > hold])


def effective(root=None, now=None):
    """{state, manual, reason, signal_at, signal_kind}: active while the newest owner signal is under WINDOW_SECS old,
    else the manual setting (read())."""
    now = time.time() if now is None else now
    manual = read(root)
    sig_at, sig_kind = last_signal(root)
    age = now - sig_at
    if manual["state"] == "mobile":
        # owner 2026-11-02: mobile is sticky; an owner turn while on a device keeps mobile (decisions go in chat).
        state, reason = "mobile", "mobile: manual setting (owner activity does not change it)"
    elif sig_at and 0 <= age < WINDOW_SECS:
        reason = f"active: {sig_kind} {max(int(age // 60), 0)} min ago"
        state = "active"
    else:
        state = manual["state"]
        reason = f"{state}: manual setting" + (f", no owner signal in {WINDOW_SECS // 60} min" if manual["state"] != "active" else "")
    return {"state": state, "manual": manual["state"], "reason": reason,
            "signal_at": _iso(sig_at) if sig_at else "", "signal_kind": sig_kind}


def record_signal(root, kind, now=None):
    """Write owner_signal_at/_kind to meta/settings (throttled to one write per SIGNAL_THROTTLE_SECS). True when written."""
    now = time.time() if now is None else now
    store = ds.Store(root or ds.DEFAULT_ROOT)
    try:
        doc = store.get("meta", "settings")
    except ds.NotFound:
        return False
    if now - _epoch(doc["data"].get("owner_signal_at")) < SIGNAL_THROTTLE_SECS or now <= _hold_until(doc["data"]):
        return False
    return _write_signal(store, doc, kind, now, now)


def sync_signal(root=None, now=None):
    """Copy the newest owner signal found in the store (answers, commands, state changes) into meta/settings when it is
    newer than the stored one, so the dashboard pane (which reads only meta/settings) can show the reason. True when written."""
    store = ds.Store(root or ds.DEFAULT_ROOT)
    sig_at, sig_kind = last_signal(root)
    try:
        doc = store.get("meta", "settings")
    except ds.NotFound:
        return False
    if not sig_at or sig_at <= _epoch(doc["data"].get("owner_signal_at")):
        return False
    return _write_signal(store, doc, sig_kind, sig_at, time.time() if now is None else now)
