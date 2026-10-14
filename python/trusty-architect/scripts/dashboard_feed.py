#!/usr/bin/env python3
"""dashboard_feed.py: per-session agent and token usage feed for the dashboard (stdlib only).

Why: the owner (2026-10-08): show active/parked subagents and token usage per session
(local/briefs/2026-10-08-dashboard-agents-tokens.md, Part B).
What: reads the token watcher's outputs (hourly rollup, open-hour state, per-file
context state, via token_watch.report/load_state) plus the mtimes and last
assistant turn of subagent transcripts, maps each Claude session id to its tmux
session name with token_watch.session_names (the watcher's own registry lookup),
and writes inbox/dashboard-feed.json = {"sessions": {<tmux name>: usage}, "fleet": usage}.
No network, no artifact call, no message text read. Does not change token_watch.
Agents: a subagent is finished when its last assistant turn has a stop_reason
other than tool_use. Unfinished and written within PARK_AFTER seconds (age <= 120)
is active; unfinished and older is parked, until PARK_MAX seconds (the watcher's
subagent-directory window), after which it is treated as abandoned.
Windows: out_tokens_1h / _24h use token_watch.report(), so they match
`token_watch.py report --since 1h` (hour buckets from hour_of(now - since)).
The fleet block sums every session, including ones with no tmux name.
Live state: fleet-poll.py passes its own pane classification (--live FILE =
{<tmux name>: {"state": working|waiting|idle|exited, "idle_min": N}}); build_feed
adds it to each session as live_state and idle_min.
apply_feed(feed, store_root) is how fleet-poll puts the feed on the dashboard with no
Architect turn: for each sessions/<S> doc whose `name` has a feed row it updates only the
`usage` field, and meta/state.fleet_usage from the fleet block, each pinned with if_version.
A row is unchanged when it equals the stored usage apart from measured_at and idle_min (they
move every cycle and the page does not show idle_min). A version conflict skips that row
until the next poll; it never overwrites. Returns {wrote, conflicts, errors}.
Accounts: latest_accounts(ledger) takes the newest line per account from
local/usage/ledger.jsonl; write_accounts(store_root, accounts) writes one accounts/<slug>
doc each (account, pct5h, reset5h, pctwk, resetwk, ts, source), only when the content
changed, so the dashboard's Accounts section follows the ledger. Nothing is invented.
CLI: `dashboard_feed.py [--out PATH] [--live FILE] [--ledger FILE] [--store-root DIR]`.
Test: scripts/test_dashboard_feed.py
"""
import argparse
import json
import os
import re
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import token_watch as tw  # noqa: E402
import dashboard_store as ds  # noqa: E402

PARK_AFTER = 120             # seconds without a write before an unfinished agent is parked
PARK_MAX = 6 * 3600          # older unfinished agents count as abandoned
TAIL = 512 * 1024
OUT_PATH = os.path.join(tw.INBOX, "dashboard-feed.json")
LEDGER = os.path.join(ds.REPO, "local", "usage", "ledger.jsonl")
ACCOUNT_FIELDS = ("account", "pct5h", "reset5h", "pctwk", "resetwk", "ts", "source")


def agent_finished(path):
    """True when the last assistant turn in the file tail ended without a tool call."""
    try:
        size = os.stat(path).st_size
        with open(path, "rb") as f:
            f.seek(max(0, size - TAIL))
            lines = f.read().splitlines()
    except OSError:
        return False
    stop = None
    for raw in lines:
        try:
            d = json.loads(raw)
        except ValueError:
            continue
        if isinstance(d, dict) and d.get("type") == "assistant":
            stop = (d.get("message") or {}).get("stop_reason")
    return stop is not None and stop != "tool_use"


def count_agents(base, now):
    """{session id: [active, parked]} from subagent transcripts."""
    out = {}
    try:
        projects = list(os.scandir(base))
    except OSError:
        return out
    for pd in projects:
        if not pd.is_dir():
            continue
        try:
            sessions = list(os.scandir(pd.path))
        except OSError:
            continue
        for sd in sessions:
            sub = os.path.join(sd.path, "subagents")
            try:
                if not sd.is_dir() or os.stat(sub).st_mtime < now - PARK_MAX:
                    continue
                files = list(os.scandir(sub))
            except OSError:
                continue
            for a in files:
                if not (a.name.startswith("agent-") and a.name.endswith(".jsonl")):
                    continue
                try:
                    age = now - a.stat().st_mtime
                except OSError:
                    continue
                if age > PARK_MAX or agent_finished(a.path):
                    continue
                c = out.setdefault(sd.name, [0, 0])
                c[0 if age <= PARK_AFTER else 1] += 1
    return out


def main_ctx(state):
    """{session id: main-thread context tokens} from the watcher's per-file state."""
    best = {}
    for f in state.get("files", {}).values():
        if f.get("kind") != "main" or not f.get("recent") or f.get("last_ts") is None:
            continue
        if f["session"] not in best or f["last_ts"] > best[f["session"]][0]:
            best[f["session"]] = (f["last_ts"], sum(f["recent"][-1]))
    return {s: v[1] for s, v in best.items()}


def iso(ts):
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def build_feed(now, base=None, names=None, state_path=None, hourly_path=None, live=None):
    base = base or tw.TRANSCRIPT_BASE
    names = tw.session_names() if names is None else names
    state = tw.load_state(state_path)
    r1 = tw.report(3600, hourly_path=hourly_path, state_path=state_path, now=now)
    r24 = tw.report(86400, hourly_path=hourly_path, state_path=state_path, now=now)
    agents, ctx = count_agents(base, now), main_ctx(state)
    zero = {"agents_active": 0, "agents_parked": 0, "out_tokens_1h": 0,
            "out_tokens_24h": 0, "units_24h": 0.0, "ctx_k": 0.0}
    by_sid = {}

    def slot(sid):
        return by_sid.setdefault(sid, dict(zero))

    for sid in names:
        slot(sid)
    for sid, (act, park) in agents.items():
        slot(sid).update(agents_active=act, agents_parked=park)
    for rows, key in ((r1, "out_tokens_1h"), (r24, "out_tokens_24h")):
        for r in rows:
            slot(r["session"])[key] += r["out_tokens"]
    for r in r24:
        slot(r["session"])["units_24h"] += r["units"]
    for sid, c in ctx.items():
        slot(sid)["ctx_k"] = round(c / 1000, 1)

    live = live or {}
    stamp = iso(now)
    sessions, fleet = {}, {k: 0 for k in zero if k != "ctx_k"}
    for sid, u in by_sid.items():
        for k in fleet:
            fleet[k] += u[k]
        name = names.get(sid)
        if not name:
            continue
        cur = sessions.setdefault(name, dict(zero))
        for k in zero:           # several session ids under one name: sum, newest ctx wins by max
            cur[k] = max(cur[k], u[k]) if k == "ctx_k" else cur[k] + u[k]
    for name in live:
        sessions.setdefault(name, dict(zero))
    for name, u in sessions.items():
        u["live_state"] = (live.get(name) or {}).get("state", "unknown")
        u["idle_min"] = (live.get(name) or {}).get("idle_min", 0)
        u["units_24h"] = round(u["units_24h"], 2)
        u["measured_at"] = stamp
    fleet["units_24h"] = round(fleet["units_24h"], 2)
    fleet["measured_at"] = stamp
    return {"sessions": sessions, "fleet": fleet}


def slug(account):
    return re.sub(r"[^a-z0-9]+", "-", str(account).lower()).strip("-")[:80]


def latest_accounts(ledger=LEDGER):
    """{slug: doc} from the newest (by ts) ledger line per account. Unreadable or account-less
    lines are skipped; an absent ledger gives {}."""
    best = {}
    try:
        with open(ledger, encoding="utf-8") as f:
            for line in f:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(r, dict) or not r.get("account") or not slug(r["account"]):
                    continue
                k = slug(r["account"])
                if k not in best or str(r.get("ts", "")) >= str(best[k].get("ts", "")):
                    best[k] = r
    except FileNotFoundError:
        return {}
    return {k: {f: r.get(f) for f in ACCOUNT_FIELDS} for k, r in best.items()}


def write_accounts(store_root, accounts):
    """Write each accounts/<slug> doc whose content changed; returns the slugs written."""
    st = ds.Store(store_root)
    wrote = []
    for k, doc in accounts.items():
        try:
            if st.get("accounts", k)["data"] == doc:
                continue
        except ds.NotFound:
            pass
        st.set("accounts", k, doc)
        wrote.append(k)
    return wrote


VOLATILE = ("measured_at", "idle_min")


def _changed(old, row):
    """True when row differs from the stored usage apart from the VOLATILE keys."""
    strip = lambda d: {k: v for k, v in d.items() if k not in VOLATILE} if isinstance(d, dict) else d
    return strip(old) != strip(row)


def apply_feed(feed, store_root=ds.DEFAULT_ROOT):
    """Write changed feed rows to the store's `usage` fields (see the module doc).

    Returns {"wrote": [doc ids, "meta/state"], "conflicts": [doc ids], "errors": [text]}.
    A store error on one document is recorded and the rest still run."""
    st = ds.Store(store_root)
    out = {"wrote": [], "conflicts": [], "errors": []}

    def put(coll, doc_id, label, field, row, version):
        try:
            st.update(coll, doc_id, {field: row}, if_version=version)
            out["wrote"].append(label)
        except (ds.Conflict, ds.NotFound):
            out["conflicts"].append(label)

    try:
        docs = st.list_docs("sessions")
    except ds.StoreError as e:
        out["errors"].append(str(e))
        docs = []
    rows = feed.get("sessions", {})
    for d in docs:
        row = rows.get(d["data"].get("name"))
        if row is not None and _changed(d["data"].get("usage"), row):
            try:
                put("sessions", d["id"], d["id"], "usage", row, d["version"])
            except (ds.StoreError, OSError) as e:
                out["errors"].append(f"sessions/{d['id']}: {e}")
    fleet = feed.get("fleet")
    if fleet:
        try:
            meta = st.get("meta", "state")
            if _changed(meta["data"].get("fleet_usage"), fleet):
                put("meta", "state", "meta/state", "fleet_usage", fleet, meta["version"])
        except ds.NotFound:
            pass
        except (ds.StoreError, OSError) as e:
            out["errors"].append(f"meta/state: {e}")
    return out


def write_feed(feed, out):
    os.makedirs(os.path.dirname(out), exist_ok=True)
    tmp = out + ".tmp"
    with open(tmp, "w") as f:
        json.dump(feed, f, indent=1, sort_keys=True)
    os.replace(tmp, out)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out", default=OUT_PATH)
    ap.add_argument("--ledger", default=LEDGER)
    ap.add_argument("--store-root", default=ds.DEFAULT_ROOT)
    ap.add_argument("--live", help="JSON file of per-session live state from fleet-poll.py")
    a = ap.parse_args(argv)
    live = None
    if a.live:
        try:
            with open(a.live) as f:
                live = json.load(f)
        except (OSError, ValueError) as e:
            print(f"dashboard_feed: live file unreadable ({e}); live_state unknown", file=sys.stderr)
    feed = build_feed(time.time(), live=live)
    write_feed(feed, a.out)
    try:
        wrote = write_accounts(a.store_root, latest_accounts(a.ledger))
        if wrote:
            print(f"dashboard_feed: accounts written: {', '.join(sorted(wrote))}")
    except (OSError, ds.StoreError) as e:
        print(f"dashboard_feed: accounts not written ({e})", file=sys.stderr)
    print(f"dashboard_feed: {len(feed['sessions'])} sessions -> {a.out} fleet={json.dumps(feed['fleet'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
