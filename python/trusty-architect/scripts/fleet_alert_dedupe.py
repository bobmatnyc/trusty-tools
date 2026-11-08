"""fleet_alert_dedupe.py — host-alert severity, dedupe and hold gating for fleet-poll.py (ar-T8a).

Why: an unchanged swap alert (constant fingerprint "swap", re-emitted every 30 min) woke a paused
Architect 16 times between 14:21Z and 21:52Z on 2026-11-08. load and disk had the same constant key.
What: swap_alert/load_alert/disk_alert build a host alert with a severity (info, warn, critical) and a
value bucket; its key is kind|severity|bucket. Swap counts only while swap-in/out is active (ruling
Df4); leftover swap is "info", which is logged and never alerts. dedup() keys these classes by
session|pane|kind and re-fires only on a severity rise, a bucket rise, a recurrence after >= 2 clear
cycles, or the 24 h reminder (not while held); every other kind keeps the pre-ar-T8a rule. gate()
holds non-urgent new alerts while the Architect is held (alert_triage.is_urgent always wakes) and
emits one held_digest on release. fleet-classify.py loads this file by path; its dedup() calls here.
Test: tests/test_fleet_poll_alert_dedupe.py
"""
import importlib.util
import os

HERE = os.path.dirname(os.path.abspath(__file__))


def _load_by_path(name, filename):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


alert_triage = _load_by_path("alert_triage", "alert_triage.py")

CLASSED = ("swap", "load", "disk")   # host alerts keyed by kind|severity|bucket
SEV_RANK = {"info": 0, "warn": 1, "critical": 2}
CLEAR_CYCLES = 2                     # a class must be absent this many cycles before it can re-fire
REMIND_S = 86400                     # the max-silence reminder for a classed alert
SWAP_BUCKET_MB = 1024
DISK_CRITICAL = 95


def _classed(alert_fn, kind, sev, bucket, detail, now):
    a = alert_fn(kind, f"{sev}|b{bucket}", detail, now=now)
    a["severity"], a["bucket"] = sev, bucket
    return a


def swap_activity(prev, cur):
    """True/False when swap-in/out moved between two (ins, outs) samples; None when either is unknown
    or the counters went backwards (a reboot)."""
    if not prev or not cur:
        return None
    moved = (cur[0] + cur[1]) - (prev[0] + prev[1])
    return None if moved < 0 else moved > 0


def swap_alert(alert_fn, reading, prev_counters, now):
    """The swap alert for one probe reading, or None when no swap is in use.

    What: reading = {"used_mb", "counters": (ins, outs) | None, "pressure": "critical"|... | None,
    "errors": [str]}. Active (counters moved since `prev_counters`) and OS pressure critical: critical.
    Active otherwise: warn; an unknown pressure never becomes critical. Not active, or activity unknown:
    info, which the poller logs and never alerts on. bucket = used MB // 1024.
    Test: test_leftover_swap_never_wakes_and_active_swap_wakes_once,
    test_a_pressure_probe_error_neither_invents_critical_nor_drops_warn
    """
    used = reading.get("used_mb")
    if not used:
        return None
    bucket = int(used // SWAP_BUCKET_MB)
    errs = "; ".join(reading.get("errors") or [])
    note = f" ({errs})" if errs else ""
    active = swap_activity(prev_counters, reading.get("counters"))
    if not active:
        state = "no swap-in/out" if active is False else "swap-in/out unknown"
        return _classed(alert_fn, "swap", "info", bucket, f"swap leftover: {used:.0f} MB, {state}{note}", now)
    if reading.get("pressure") == "critical":
        return _classed(alert_fn, "swap", "critical", bucket,
                        f"swap active: {used:.0f} MB, memory pressure critical{note}", now)
    return _classed(alert_fn, "swap", "warn", bucket,
                    f"swap active: {used:.0f} MB, memory pressure {reading.get('pressure') or 'unknown'}{note}", now)


def load_alert(alert_fn, loads, cores, factor, now):
    """warn when the 5-min average is over factor x cores; bucket = 5-min load per core, floored."""
    load1, load5, load15 = loads
    if load5 <= factor * cores:
        return None
    return _classed(alert_fn, "load", "warn", int(load5 // cores),
                    f"load {load1:.1f}/{load5:.1f}/{load15:.1f} (5m > {factor}x{cores} cores)", now)


def disk_alert(alert_fn, pct, high, now):
    """warn over `high` percent, critical over DISK_CRITICAL; bucket = whole percent used."""
    if pct <= high:
        return None
    sev = "critical" if pct > DISK_CRITICAL else "warn"
    return _classed(alert_fn, "disk", sev, int(pct), f"root disk {pct:.0f}% used", now)


def _migrate(seen, ck, a):
    """A pre-ar-T8a entry ({first, last} under ck|<fingerprint>) adopted at the current bucket and at most
    warn, so the first cycle after an upgrade does not re-wake, and a critical still does."""
    for k, v in seen.items():
        if k.startswith(ck + "|") and isinstance(v, dict) and "sev" not in v and "last" in v:
            sev = "warn" if a["severity"] == "critical" else a["severity"]
            return {"first": v.get("first", v["last"]), "last": v["last"], "sev": sev,
                    "bucket": a["bucket"], "last_wake": v["last"], "clear_n": 0}
    return None


def _decide_classed(prev, a, now, every, held):
    """(fire, entry) for one classed alert against its stored entry (None: first sight)."""
    sev, bucket = a["severity"], a["bucket"]
    key = f"{a['kind']}|{sev}|b{bucket}"
    if prev is None:
        return True, {"first": now, "last": now, "sev": sev, "bucket": bucket, "last_wake": now,
                      "clear_n": 0, "key": key}
    rise = SEV_RANK[sev] > SEV_RANK[prev["sev"]] or bucket > prev["bucket"]
    remind = not held and now - prev["last_wake"] >= every
    if rise or remind:
        top = sev if SEV_RANK[sev] >= SEV_RANK[prev["sev"]] else prev["sev"]
        return True, {"first": prev["first"], "last": now, "sev": top, "bucket": max(bucket, prev["bucket"]),
                      "last_wake": now, "clear_n": 0, "key": key}
    return False, {**prev, "last": now, "clear_n": 0}


def dedup(state, candidates, now, reemit, held=False):
    """Split candidates into (new, suppressed_count) and update state['alerts'].

    Classed kinds (CLASSED, carrying "severity"): key session|pane|kind; see the module doc. An entry absent
    for a cycle stays with clear_n counting; at CLEAR_CYCLES it goes. "info" alerts are skipped here.
    Other kinds: key session|pane|kind|fingerprint, cleared after one absent cycle, re-emitted after
    reemit[kind] seconds. A stored entry that cannot be read counts as absent, so the alert fires.
    Test: test_restart_and_old_shape_state_do_not_re_wake, test_clear_for_two_cycles_then_recur_wakes,
    test_the_24h_reminder_fires_once_and_never_while_held
    """
    seen = state.get("alerts")
    seen = seen if isinstance(seen, dict) else {}
    active, new, suppressed = {}, [], 0
    for a in candidates:
        if a.get("severity") == "info":
            continue
        if a["kind"] in CLASSED and "severity" in a:
            k = "|".join((a["session"], a["pane"], a["kind"]))
            if k in active:
                continue
            try:
                prev = seen.get(k) if isinstance(seen.get(k), dict) else _migrate(seen, k, a)
                fire, entry = _decide_classed(prev, a, now, reemit.get(a["kind"], REMIND_S), held)
            except (KeyError, TypeError, ValueError):   # unreadable entry: fail open
                fire, entry = _decide_classed(None, a, now, REMIND_S, held)
        else:
            k = "|".join((a["session"], a["pane"], a["kind"], a["fingerprint"]))
            if k in active:
                continue
            prev, every = seen.get(k), reemit.get(a["kind"])
            prev = prev if isinstance(prev, dict) else None
            try:
                fire = prev is None or bool(every and now - prev["last"] >= every)
                entry = {"first": prev["first"] if prev else now, "last": now} if fire else prev
            except (KeyError, TypeError, ValueError):
                fire, entry = True, {"first": now, "last": now}
        active[k] = entry
        if fire:
            new.append(a)
        else:
            suppressed += 1
    for k, v in seen.items():
        if k not in active and isinstance(v, dict) and "clear_n" in v:
            n = v["clear_n"] + 1 if isinstance(v["clear_n"], int) else CLEAR_CYCLES
            if n < CLEAR_CYCLES:
                active[k] = {**v, "clear_n": n}
    state["alerts"] = active
    return new, suppressed


def gate(state, new, hold, now, alert_fn):
    """(wake, held) from this cycle's new alerts and read_hold()'s result.

    While held: urgent alerts (alert_triage.is_urgent) and the hold's `allow` kinds wake; the rest get a
    "held" field, are counted by kind in state["held_counts"] and wake nothing. Not held: everything new
    wakes, plus ONE held_digest alert when the hold just ended with alerts counted.
    Test: test_a_hold_logs_non_urgent_and_wakes_urgent, test_release_emits_one_digest_and_no_replay
    """
    counts = state.get("held_counts")
    counts = counts if isinstance(counts, dict) else {}
    if hold.get("held"):
        allow = hold.get("allow") or set()
        wake = [a for a in new if alert_triage.is_urgent(a) or a["kind"] in allow]
        held = [a for a in new if not (alert_triage.is_urgent(a) or a["kind"] in allow)]
        for a in held:
            a["held"] = hold.get("reason") or "held"
            counts[a["kind"]] = counts.get(a["kind"], 0) + 1
        state["held_counts"] = counts
        return wake, held
    state.pop("held_counts", None)
    total = sum(n for n in counts.values() if isinstance(n, int))
    if total:
        detail = f"{total} alerts held: " + ", ".join(f"{k} {n}" for k, n in sorted(counts.items()))
        new = new + [alert_fn("held_digest", str(int(now)), detail, now=now)]
    return new, []
