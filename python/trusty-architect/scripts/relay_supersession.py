#!/usr/bin/env python3
"""relay_supersession.py: the owner-ruling supersession check relay_provenance.check runs after verify (#10960).

Why: an authentic quote of an owner turn the owner has since reversed still passed the relay check (DOC-87 G-4);
this split keeps relay_provenance.py under the 500-line cap.
What: ledger_items reads every Architect IL and IL_SCHEDULE ledger (state.json and archive.jsonl) and reports
damaged records; superseded gives the verdict for a cite whose owner turn is a ruling marked `superseded_by`;
touches says whether a damaged record may hold a link on the cite's chain. relay_provenance imports these at
load, so a bundle without this file fails the relay check rather than skipping it.
Test: tests/test_relay_supersession.py
"""
import json
import os
import re
from datetime import datetime, timedelta, timezone

from relay_provenance import (CHAT, DASH, ROOT, WINDOW, _norm, _ts, chat_records, dashboard_turns,
                              quote_problem)


def _ledger_paths(damage):
    """state.json paths of the Architect IL and every IL_SCHEDULE session; an unreadable schedule is bytes-less damage."""
    import dashboard_il as dil
    sched = os.environ.get("IL_SCHEDULE") or os.path.join(ROOT, "local", "il-schedule.json")
    try:
        with open(sched, "rb") as f:
            json.loads(f.read().decode("utf-8"))
    except FileNotFoundError:
        pass
    except (OSError, ValueError) as e:   # also UnicodeDecodeError
        damage.append({"path": sched, "reason": f"cannot be read ({e}), so its sessions' ledgers were not read",
                       "raw": None})
    arch = os.environ.get("ARCHITECT_IL") or os.path.join(ROOT, "local", "il")
    return [r["state_path"] for r in dil.discover({}, sched, arch)]


def _read(path, damage):
    """The bytes of `path`; None when it does not exist, or when it cannot be read (recorded in `damage`)."""
    try:
        with open(path, "rb") as f:
            return f.read()
    except FileNotFoundError:
        return None
    except OSError as e:
        damage.append({"path": path, "reason": f"cannot be read ({e})", "raw": None})
        return None


def ledger_items():
    """(items, damage): every ledger item, open (state.json) and archived (archive.jsonl), and each damaged record as
    {path, reason, raw: its bytes, or None when unreadable}. A missing file holds no link and is not damage."""
    items, damage = [], []
    for sp in _ledger_paths(damage):
        raw = _read(sp, damage)
        if raw is not None:
            try:
                got = json.loads(raw.decode("utf-8")).get("items")
                if not isinstance(got, list):
                    raise ValueError("no items list")
                items += [i for i in got if isinstance(i, dict)]
            except (ValueError, AttributeError) as e:
                damage.append({"path": sp, "reason": f"is corrupt ({e})", "raw": raw})
        ap = os.path.join(os.path.dirname(sp), "archive.jsonl")
        for n, ln in enumerate((_read(ap, damage) or b"").splitlines(), 1):
            try:
                item = json.loads(ln.decode("utf-8")).get("item")
            except (ValueError, AttributeError):
                damage.append({"path": f"{ap}:{n}", "reason": "is a corrupt archive line", "raw": ln})
                continue
            if isinstance(item, dict):
                items.append(item)
    return items, damage


def touches(d, cite, quotes, chain, located):
    """True when the damaged record `d` may hold a link on this cite's chain (#10960, Di9).

    What: with its bytes, when they name the cite's turn (`chat YYYY-MM-DDTHH:MM` for any minute within 2 of the
    cited time or of a turn the quote matched; `dashboard [S..:]<id>`) or quote an id on the cite's chain. Bytes
    lost to the damage hold no link, as a missing ledger holds none. Without bytes (unreadable file or schedule):
    unless a sound record located the cite, since its links live beside it in that ledger.
    Test: test_relay_refuses_a_damaged_record_on_the_cites_chain_10960,
    test_relay_flags_a_damaged_ledger_off_the_cites_chain_and_sends_10960
    """
    if d["raw"] is None:
        return not located
    pats = [b'"' + re.escape(i.encode()) + b'"' for i in chain if isinstance(i, str)]
    m, dash = CHAT.match(cite), DASH.match(cite)
    if m:
        for t in filter(None, [_ts(m.group(1))] + [_ts(k[1]) for k in turn_keys(cite, quotes)]):
            pats += [re.escape(("chat " + (t + timedelta(minutes=k)).strftime("%Y-%m-%dT%H:%M")).encode())
                     for k in range(-2, 3)]
    elif dash:
        pats.append(rb"dashboard (?:S[0-9a-f]{2}:)?" + re.escape(dash.group(1).split(":")[-1].encode()) +
                    rb"(?![\w.-])")
    return bool(pats) and re.search(b"|".join(pats), d["raw"]) is not None


def turn_keys(cite, quotes):
    """The owner turns of `cite` that hold one of `quotes`: ('chat', at, text) or ('dashboard', id, text)."""
    m, d = CHAT.match(cite or ""), DASH.match(cite or "")
    if m:
        recs = [("chat", at, t) for at, t in chat_records(m.group(1))]
    elif d:
        recs = [("dashboard", d.group(1).split(":")[-1], t) for t in dashboard_turns(d.group(1))]
    else:
        return set()
    return {k for k in recs if any(isinstance(q, str) and quote_problem(q, [k[2]]) is None for q in quotes)}


def _src(item):
    return item.get("source") if isinstance(item.get("source"), dict) else {}


def _maybe(icite, cite):
    """Cheap pre-check: can the ledger cite `icite` name the same owner turn as `cite`?"""
    a, b = CHAT.match(icite), CHAT.match(cite)
    if a and b:
        ta, tb = _ts(a.group(1)), _ts(b.group(1))
        return bool(ta and tb) and abs(ta - tb) <= 3 * WINDOW
    a, b = DASH.match(icite), DASH.match(cite)
    return bool(a and b) and a.group(1).split(":")[-1] == b.group(1).split(":")[-1] or _norm(icite) == _norm(cite)


def _hops(old, by_id):
    """(the successors of `old` in order, the id the chain breaks at: one no ledger holds, or a loop; else None)."""
    hops, seen, cur = [], {old.get("id")}, old
    while "superseded_by" in cur:
        nxt = cur["superseded_by"]
        succ = by_id.get(nxt) if isinstance(nxt, str) else None
        if succ is None or nxt in seen:
            return hops, nxt
        hops.append(succ)
        seen.add(nxt)
        cur = succ
    return hops, None


def _verdict(old, hops, broken, cite, now):
    """(refuse, message) for a cite of the superseded ruling `old`. It only flags (DOC-87 X11) when every successor
    is bounded and the last one's `until` is still ahead; an unbounded end or a passed `until` refuses."""
    head = f"the owner turn `{cite}` is ledger ruling {old.get('id')}, superseded by "
    if broken is not None:
        return True, head + (f"{old['superseded_by']!r}; the chain breaks at {broken!r}, which no ledger holds; ask "
                             "the owner for the current ruling")
    s, last = _src(hops[0]), hops[-1]
    msg = (head + f"{hops[0].get('id')} \"{s.get('quote')}\" (owner turn: {s.get('cite')})"
           + (f"; the current ruling is {last.get('id')}" if last is not hops[0] else "") + "; cite the current ruling")
    untils = [_ts(_src(h).get("until")) for h in hops]
    if all(untils) and untils[-1] > now:
        return False, msg + f" (every successor is bounded, the last until {_src(last)['until']}; flagged, not refused)"
    return True, msg + (f" (its until {_src(last)['until']} has passed)" if untils[-1] else "")


def superseded(cite, quotes, items, now=None):
    """(refuse, message, chain, located) for an owner cite against the ledger items (#10960 G-4).

    What: an item matches when its owner turn is the cite's (turn_keys, or the cite text when the turn is gone);
    `located` says one did. A matched superseded item gives the verdict (_verdict; a refusal wins); message is None
    when none is superseded. `chain` holds the matched and successor ids, for touches.
    Test: test_relay_refuses_a_cite_of_a_superseded_owner_ruling_10960,
    test_relay_flags_a_cite_superseded_by_a_bounded_ruling_10960,
    test_relay_refuses_past_a_bounded_successor_to_an_unbounded_one_10960
    """
    now = now or datetime.now(timezone.utc)
    by_id, mine, chain, located, verdict = {i.get("id"): i for i in items}, turn_keys(cite, quotes), set(), False, None
    for it in items:
        src = _src(it)
        if not isinstance(src.get("cite"), str) or not _maybe(src["cite"], cite):
            continue
        keys = turn_keys(src["cite"], [src.get("quote")])
        if not (keys & mine if keys else _norm(src["cite"]) == _norm(cite)):
            continue
        located = True
        chain.add(it.get("id"))
        if "superseded_by" in it:
            hops, broken = _hops(it, by_id)
            chain.update([h.get("id") for h in hops] + [broken])
            v = _verdict(it, hops, broken, cite, now)
            verdict = v if verdict is None or (v[0] and not verdict[0]) else verdict
    return (verdict or (False, None)) + (chain, located)
