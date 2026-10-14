#!/usr/bin/env python3
"""token_watch.py: deterministic fleet token watcher (no LLM, stdlib only).

Why: the owner (2026-10-07): "only inference if action needed". The watcher decides
when token behaviour needs attention; the token-optimizer agent then suggests.
Spec: local/briefs/2026-10-07-token-watch-spec.md.
What: reads Claude Code transcripts incrementally (main and subagent files of
every project directory), de-duplicates turns by message.id, applies the rules
in DEFAULTS, returns alert dicts for fleet-poll.py and appends an hourly
rollup to local/usage/token-hourly.jsonl. Never reads or stores message text:
only usage numbers, model, timestamps, ids and the Bash command shape of
polling turns. State: inbox/token-watch-state.json (offsets, counters, fired
fingerprints). Alert dedup here: an exact fingerprint fires once per 24 h, and
a rule's COOLDOWN keeps hourly-bucketed fingerprints from re-firing each hour.
CLI: `token_watch.py report --since 1h [--json]`, `token_watch.py scan --dry-run`.
Test: scripts/test_token_watch.py
"""
import argparse
import copy
import json
import os
import re
import statistics
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
INBOX = os.environ.get("SUPERVISOR_INBOX_DIR") or os.path.join(ROOT, "inbox")
STATE_PATH = os.path.join(INBOX, "token-watch-state.json")
HOURLY_PATH = os.path.join(ROOT, "local", "usage", "token-hourly.jsonl")
THRESHOLDS_PATH = os.path.join(ROOT, "local", "usage", "token-thresholds.json")
TRANSCRIPT_BASE = os.environ.get("TRANSCRIPT_BASE") or os.path.expanduser(
    "~/.trusty-tools/trusty-mpm/claude-config/projects")
SESSIONS_DIR = os.environ.get("SESSIONS_DIR") or os.path.expanduser(
    "~/.trusty-tools/trusty-mpm/claude-config/sessions")

DEFAULTS = {
    "ctx_main": [200_000, 250_000, 300_000, 400_000],
    "ctx_sub": [150_000, 250_000],
    "hit_low": 0.90,
    "miss_unexpected": 100_000,
    "miss_idle": 150_000,
    "poll_agent": 12,
    "poll_rate_factor": 2.0,
    "poll_rate_min": 60,
    "opus_share": 0.80,
    "fanout_cap": 3,           # 1 while pacing b5 is active: set it in the thresholds file
    "burn_factor": 1.5,
    "session_start_churn": 6,
    "ttl_main": 3600,
    "ttl_sub": 300,
    "w_read_opus": 0.05,
    "w_read_other": 0.10,
    "rewrite_frac": 0.5,
    "rewrite_min_ctx": 50_000,
    "miss_burst_cap": 3,
}
COOLDOWN = {"hit_low": 21600, "poll_rate": 21600, "opus_share": 21600, "burn": 21600,
            "session_start_churn": 21600, "fanout": 3600, "watcher_degraded": 21600}
LOOKBACK = 3600            # first-sight lookback and file mtime cutoff
FIRED_TTL = 86400
BUDGET_S = 20.0            # hard cap per scan; resumes next cycle
READ_CAP = 16 * 1024 * 1024
FIRST_TAIL = 4 * 1024 * 1024
PRICE = {"opus": 5.0, "sonnet": 3.0, "haiku": 1.0}
POLL_RE = re.compile(r"^\s*tm wait\b|\bsleep \d+")
# The home directory's prefix on a Claude Code project dir name ("/a/b" -> "-a-b-").
SESSION_DIR_RE = re.compile("^" + re.escape(re.sub(r"[^A-Za-z0-9]", "-", os.path.expanduser("~"))) + "-")


def thresholds(path=None):
    t = dict(DEFAULTS)
    try:
        with open(path or THRESHOLDS_PATH) as f:
            t.update(json.load(f))
    except (OSError, ValueError):
        pass
    return t


def parse_ts(s):
    try:
        return datetime.fromisoformat(str(s).replace("Z", "+00:00")).timestamp()
    except (ValueError, TypeError):
        return None


def hour_of(ts):
    return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%dT%H")


def hour_fp(ts):
    return hour_of(ts).replace("-", "").replace("T", "")


def family(model):
    m = (model or "").lower()
    return next((k for k in PRICE if k in m), "sonnet")


def turn_units(model, u):
    """Spec proxy: price x (in + 5 out + w_read x read + 1.25 c5m + 2 c1h) / 1e6."""
    fam = family(model)
    cc = u.get("cache_creation") or {}
    c1h = cc.get("ephemeral_1h_input_tokens") or 0
    c5 = cc.get("ephemeral_5m_input_tokens")
    if c5 is None:
        c5 = max(0, (u.get("cache_creation_input_tokens") or 0) - c1h)
    w = DEFAULTS["w_read_opus"] if fam == "opus" else DEFAULTS["w_read_other"]
    raw = ((u.get("input_tokens") or 0) + 5 * (u.get("output_tokens") or 0)
           + w * (u.get("cache_read_input_tokens") or 0) + 1.25 * c5 + 2.0 * c1h)
    return PRICE[fam] * raw / 1e6


def pct(vals, p):
    if not vals:
        return 0
    s = sorted(vals)
    return s[min(len(s) - 1, int(p * len(s)))]


def new_state():
    return {"files": {}, "seen": [], "open": {}, "fleet": {}, "fired": {}, "starts": {},
            "miss_count": {}, "parse_errors": 0, "lines": 0}


def load_state(path=None):
    try:
        with open(path or STATE_PATH) as f:
            st = json.load(f)
        for k, v in new_state().items():
            st.setdefault(k, v)
        return st
    except (OSError, ValueError):
        return new_state()


def save_state(st, path=None):
    path = path or STATE_PATH
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(st, f)
    os.replace(tmp, path)


# --- discovery ---------------------------------------------------------------

def discover(base, now):
    """(path, kind, project, sid, agent_id) for transcript files modified in the
    lookback. scandir only: a project directory holds thousands of files."""
    cutoff = now - LOOKBACK
    out = []
    try:
        projects = list(os.scandir(base))
    except OSError:
        return out
    for pd in projects:
        if not pd.is_dir():
            continue
        try:
            entries = list(os.scandir(pd.path))
        except OSError:
            continue
        for e in entries:
            try:
                if e.is_file() and e.name.endswith(".jsonl"):
                    if e.stat().st_mtime >= cutoff:
                        out.append((e.path, "main", pd.name, e.name[:-6], ""))
                elif e.is_dir():
                    sub = os.path.join(e.path, "subagents")
                    if os.stat(sub).st_mtime < now - 6 * 3600:
                        continue
                    for a in os.scandir(sub):
                        if a.name.startswith("agent-") and a.name.endswith(".jsonl") \
                                and a.stat().st_mtime >= cutoff:
                            out.append((a.path, "sub", pd.name, e.name, a.name[6:-6]))
            except OSError:
                continue
    return out


def agent_type(path):
    try:
        with open(path[:-len(".jsonl")] + ".meta.json") as f:
            return str(json.load(f).get("agentType") or "subagent")
    except (OSError, ValueError):
        return "subagent"


def session_names(sessions_dir=None):
    """sessionId -> tmux session name from the live-session registry."""
    names = {}
    try:
        for n in os.listdir(sessions_dir or SESSIONS_DIR):
            if not n.endswith(".json"):
                continue
            try:
                with open(os.path.join(sessions_dir or SESSIONS_DIR, n)) as f:
                    d = json.load(f)
                tmux = str(d.get("tmux", ""))
                if d.get("sessionId") and ":" in tmux:
                    names[d["sessionId"]] = tmux.split(":", 1)[0]
            except (OSError, ValueError):
                continue
    except OSError:
        pass
    return names


# --- engine ------------------------------------------------------------------

class Scan:
    def __init__(self, st, th, now, base, names):
        self.st, self.th, self.now, self.base, self.names = st, th, now, base, names
        self.alerts = []
        self.seen = set(st["seen"])
        self.lines = self.errors = 0
        self.min_ts = 0

    def label(self, f):
        return self.names.get(f["session"]) or \
            f"{SESSION_DIR_RE.sub('', f['project'])[:24]}:{f['session'][:8]}"

    def emit(self, f, rule, fp, detail, scope=None):
        scope = scope or f["session"]
        key, rkey = f"{scope}|{fp}", f"{scope}|rule:{rule}"
        fired, now = self.st["fired"], self.now
        cool = COOLDOWN.get(rule, 0)
        if key in fired or (cool and now - fired.get(rkey, 0) < cool):
            return
        fired[key] = fired[rkey] = now
        self.alerts.append({"rule": rule, "session": self.label(f) if f else scope,
                            "fingerprint": fp, "detail": f"{rule}: {detail}"})

    def tag(self, f):
        return f"{f['kind'] if f['kind'] == 'main' else f['agent_type']}"

    def rel(self, path):
        return os.path.relpath(path, self.base) if path.startswith(self.base) else path

    # one assistant line -> state, hourly accumulators, per-turn rules
    def turn(self, path, f, d):
        msg = d.get("message") or {}
        u = msg.get("usage")
        ts = parse_ts(d.get("timestamp"))
        if not isinstance(u, dict) or ts is None or ts < self.min_ts:
            return
        mid = msg.get("id") or d.get("uuid")
        if mid:
            if mid in self.seen:
                return
            self.seen.add(mid)
            self.st["seen"].append(mid)
        th, model = self.th, msg.get("model") or ""
        rd, cc, inp = (u.get("cache_read_input_tokens") or 0, u.get("cache_creation_input_tokens") or 0,
                       u.get("input_tokens") or 0)
        out_t, ctx = u.get("output_tokens") or 0, rd + cc + inp
        gap = None if f.get("last_ts") is None else ts - f["last_ts"]
        f["last_ts"] = ts
        units = turn_units(model, u)
        fam = family(model)
        atype = "main" if f["kind"] == "main" else f["agent_type"]
        if f["kind"] == "main" and not f.get("started"):
            f["started"] = True
            if f.get("off0"):
                self.st["starts"].setdefault(f["project"], []).append([ts, f["session"]])
        # polling turn (subagents only; command shape only, never output)
        blocks = [b for b in (msg.get("content") or []) if isinstance(b, dict) and b.get("type") == "tool_use"]
        poll = (f["kind"] == "sub" and len(blocks) == 1 and blocks[0].get("name") == "Bash"
                and bool(POLL_RE.search(str((blocks[0].get("input") or {}).get("command", "")))))
        rec = f["recent"]
        rec.append([rd, cc, inp])
        del rec[:-20]
        rewrite = cc > th["rewrite_frac"] * ctx and ctx > th["rewrite_min_ctx"]
        expected = unexpected = False
        if rewrite and gap is not None:
            expected = gap > (th["ttl_main"] if f["kind"] == "main" else th["ttl_sub"])
            unexpected = not expected
        # hourly accumulator
        h = hour_of(ts)
        k = "|".join((h, f["project"], f["session"], atype, model))
        o = self.st["open"].setdefault(k, {"turns": 0, "ctx": [], "out": 0, "rd": 0, "cc": 0, "in": 0,
                                           "rw_exp": 0, "rw_unexp": 0, "polls": 0, "units": 0.0})
        o["turns"] += 1
        o["ctx"].append(ctx)
        o["out"] += out_t
        o["rd"] += rd
        o["cc"] += cc
        o["in"] += inp
        o["rw_exp"] += int(expected)
        o["rw_unexp"] += int(unexpected)
        o["polls"] += int(poll)
        o["units"] += units
        fl = self.st["fleet"].setdefault(h, {"units": 0.0, "opus": 0.0, "polls": 0})
        fl["units"] += units
        fl["opus"] += units if fam == "opus" else 0
        fl["polls"] += int(poll)
        ident = f["agent_id"][:8] if f["kind"] == "sub" else f["session"][:8]
        who = f"{self.tag(f)} {model.split('-')[1] if '-' in model else model}"
        # ctx buckets
        rule = "ctx_main" if f["kind"] == "main" else "ctx_sub"
        crossed = [b for b in th[rule] if ctx >= b]
        for b in crossed[:-1]:           # a jump past several buckets alerts once, at the top one
            self.st["fired"].setdefault(f"{f['session']}|{rule}:{b // 1000}k", self.now)
        if crossed:
            b = crossed[-1]
            self.emit(f, rule, f"{rule}:{b // 1000}k",
                      f"ctx {ctx} >= {b} {who} out={out_t} {self.rel(path)}")
        # hit ratio
        if len(rec) >= 20 and ctx > th["rewrite_min_ctx"]:
            tot = sum(sum(r) for r in rec)
            ratio = sum(r[0] for r in rec) / tot if tot else 1.0
            if ratio < th["hit_low"]:
                self.emit(f, "hit_low", f"hit_low:{ident if f['kind'] == 'sub' else 'main'}:{hour_fp(ts)}",
                          f"hit {ratio:.2f} < {th['hit_low']} over 20 turns ctx={ctx} {who} {self.rel(path)}")
        # cache misses
        if unexpected and cc >= th["miss_unexpected"]:
            n = self.st["miss_count"].get(f"{f['session']}|{h}", 0) + 1
            self.st["miss_count"][f"{f['session']}|{h}"] = n
            if n <= th["miss_burst_cap"]:
                self.emit(f, "miss_unexpected", f"miss_unexpected:{(mid or ident)[:8]}",
                          f"rewrote {cc} tokens after {gap:.0f}s gap (ttl {th['ttl_sub'] if f['kind'] == 'sub' else th['ttl_main']}) ctx={ctx} {who} {self.rel(path)}")
            elif n == th["miss_burst_cap"] + 1:
                self.emit(f, "miss_unexpected", f"miss_unexpected:burst:{hour_fp(ts)}",
                          f"burst: over {th['miss_burst_cap']} unexpected rewrites this hour {who} {self.rel(path)}")
        if expected and cc >= th["miss_idle"] and fam == "opus":
            self.emit(f, "miss_idle", f"miss_idle:{ident}",
                      f"rewrote {cc} tokens after {gap:.0f}s idle gap ctx={ctx} {who} {self.rel(path)}")
        # polling
        if poll:
            pl = f["polls"]
            pl.append(ts)
            f["polls"] = pl = [t for t in pl if t > ts - 1800]
            if len(pl) >= th["poll_agent"]:
                self.emit(f, "poll_agent", f"poll_agent:{ident}",
                          f"{len(pl)} polling turns in 30 min >= {th['poll_agent']} {who} {self.rel(path)}")

    def read_file(self, path, f):
        try:
            stt = os.stat(path)
        except OSError:
            return
        f["seen_at"] = self.now
        first = "ino" not in f
        self.min_ts = self.now - (LOOKBACK if first else 3 * LOOKBACK)
        if not first and (f["ino"] != stt.st_ino or stt.st_size < f.get("off", 0)):
            f["off"] = 0
        off = f.get("off", 0)
        if first and stt.st_size > FIRST_TAIL:
            off = stt.st_size - FIRST_TAIL
        if off >= stt.st_size:
            f["ino"], f["size"] = stt.st_ino, stt.st_size
            return
        f.setdefault("off0", off == 0)
        with open(path, "rb") as fh:
            fh.seek(off)
            data = fh.read(min(stt.st_size - off, READ_CAP))
        end = data.rfind(b"\n")
        if end < 0:                      # only a partial line so far
            f["ino"], f["size"] = stt.st_ino, stt.st_size
            return
        chunk = data[:end + 1]
        if off > 0 and first:            # tail start: drop the cut first line
            chunk = chunk[chunk.find(b"\n") + 1:]
        for raw in chunk.splitlines():
            if not raw.strip():
                continue
            self.lines += 1
            try:
                d = json.loads(raw)
            except ValueError:
                self.errors += 1
                continue
            if isinstance(d, dict) and f.get("kind") == "sub":
                # #fanout: a subagent is done once its transcript ends on an
                # assistant turn with stop_reason end_turn; any later user or
                # assistant line (a resume) makes it live again.
                m = d.get("message") or {}
                if d.get("type") == "assistant":
                    f["done"] = m.get("stop_reason") in ("end_turn", "stop_sequence")
                elif d.get("type") == "user":
                    f["done"] = False
            if isinstance(d, dict) and (d.get("type") == "assistant"
                                        or (d.get("message") or {}).get("role") == "assistant"):
                self.turn(path, f, d)
        f.update(ino=stt.st_ino, size=stt.st_size, off=off + end + 1)

    def fleet_rules(self):
        th, now, fleet = self.th, self.now, self.st["fleet"]
        cur = hour_of(now)
        hours = sorted(fleet)
        done = [h for h in hours if h < cur]
        last = done[-1] if done else None
        # poll_rate: last completed hour vs 2x the median of the prior hours
        if last:
            prior = [fleet[h]["polls"] for h in done[:-1][-168:]]
            if len(prior) >= 24:
                lim = max(th["poll_rate_min"], th["poll_rate_factor"] * statistics.median(prior))
                n = fleet[last]["polls"]
                if n > lim:
                    self.emit(None, "poll_rate", f"poll_rate:{last.replace('-', '').replace('T', '')}",
                              f"{n} polling turns/h > {lim:.0f} (2x 7d median) hour {last}", "fleet")
        # opus_share over the last 3 h
        recent = [fleet[h] for h in hours if h >= hour_of(now - 2 * 3600)]
        tot = sum(r["units"] for r in recent)
        if tot >= 10:
            share = sum(r["opus"] for r in recent) / tot
            if share > th["opus_share"]:
                self.emit(None, "opus_share", f"opus_share:{hour_fp(now)}",
                          f"opus share {share:.2f} > {th['opus_share']} over 3h units={tot:.0f}", "fleet")
        # burn: last two completed hours both over factor x trailing 24 h mean
        if len(done) >= 8:
            two, trail = done[-2:], done[:-2][-24:]
            mean = sum(fleet[h]["units"] for h in trail) / len(trail)
            if mean > 0 and all(fleet[h]["units"] > th["burn_factor"] * mean for h in two):
                self.emit(None, "burn", f"burn:{two[-1].replace('-', '').replace('T', '')}",
                          f"fleet units/h {fleet[two[-1]]['units']:.0f} and {fleet[two[0]]['units']:.0f} > {th['burn_factor']}x 24h mean {mean:.0f}", "fleet")
        # session_start_churn
        for proj, rows in self.st["starts"].items():
            rows[:] = [r for r in rows if r[0] >= now - 3 * 3600]
            n = len({r[1] for r in rows})
            if n > th["session_start_churn"]:
                self.emit(None, "session_start_churn", f"session_start_churn:{proj}:{hour_fp(now)}",
                          f"{n} main-session starts in 3h > {th['session_start_churn']} project {SESSION_DIR_RE.sub('', proj)}",
                          SESSION_DIR_RE.sub("", proj)[:24] or "fleet")
        # fanout
        live = {}
        for p, f in self.st["files"].items():
            if f.get("kind") == "sub" and not f.get("done") and f.get("last_ts") \
                    and f["last_ts"] >= now - 300:
                live.setdefault(f["session"], []).append(f)
        for sid, fl in live.items():
            if len(fl) > th["fanout_cap"]:
                self.emit(fl[0], "fanout", f"fanout:{len(fl)}",
                          f"{len(fl)} concurrent subagents > cap {th['fanout_cap']} session {sid[:8]}")

    def degraded(self):
        if self.lines >= 20 and self.errors / self.lines > 0.05:
            self.emit(None, "watcher_degraded", f"watcher_degraded:{hour_fp(self.now)}",
                      f"{self.errors}/{self.lines} transcript lines failed to parse", "fleet")


def rollup_row(key, o):
    h, project, session, atype, model = key.split("|", 4)
    tot = o["rd"] + o["cc"] + o["in"]
    return {"hour": h, "project": project, "session": session, "agent_type": atype, "model": model,
            "turns": o["turns"], "ctx_p50": pct(o["ctx"], 0.5), "ctx_p90": pct(o["ctx"], 0.9),
            "out_tokens": o["out"], "hit_ratio": round(o["rd"] / tot, 4) if tot else None,
            "rewrites_expected": o["rw_exp"], "rewrites_unexpected": o["rw_unexp"],
            "poll_turns": o["polls"], "units": round(o["units"], 3),
            "units_opus": round(o["units"], 3) if family(model) == "opus" else 0.0}


def flush_hours(st, now, hourly_path):
    """Append completed-hour rows; a key already in the file is skipped."""
    cur = hour_of(now)
    due = [k for k in st["open"] if k.split("|", 1)[0] < cur]
    if not due:
        return 0
    have = set()
    try:
        with open(hourly_path) as f:
            for line in f:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                have.add("|".join((r["hour"], r["project"], r["session"], r["agent_type"], r["model"])))
    except OSError:
        pass
    os.makedirs(os.path.dirname(hourly_path), exist_ok=True)
    n = 0
    with open(hourly_path, "a") as f:
        for k in sorted(due):
            if k not in have:
                f.write(json.dumps(rollup_row(k, st["open"][k])) + "\n")
                n += 1
            del st["open"][k]
    return n


def prune(st, now):
    st["seen"] = st["seen"][-5000:]
    st["files"] = {p: f for p, f in st["files"].items() if now - f.get("seen_at", now) < 86400}
    st["fired"] = {k: t for k, t in st["fired"].items() if now - t < FIRED_TTL}
    st["fleet"] = {h: v for h, v in st["fleet"].items() if h >= hour_of(now - 8 * 86400)}
    st["miss_count"] = {k: v for k, v in st["miss_count"].items() if k.rsplit("|", 1)[1] >= hour_of(now - 7200)}


def step(now=None, dry_run=False, base=None, state_path=None, hourly_path=None,
         thresholds_path=None, names=None):
    """One scan cycle. Returns [{"rule","session","fingerprint","detail"}].
    dry_run works on a copy and writes nothing."""
    now = time.time() if now is None else now
    base = base or TRANSCRIPT_BASE
    st = load_state(state_path)
    if dry_run:
        st = copy.deepcopy(st)
    sc = Scan(st, thresholds(thresholds_path), now, base, names if names is not None else session_names())
    t0 = time.monotonic()
    for path, kind, project, sid, aid in sorted(discover(base, now)):
        if time.monotonic() - t0 > BUDGET_S:
            print("token_watch: time budget reached, resuming next cycle", file=sys.stderr)
            break
        f = st["files"].setdefault(path, {"kind": kind, "project": project, "session": sid,
                                          "agent_id": aid, "last_ts": None, "recent": [], "polls": []})
        if kind == "sub" and "agent_type" not in f:
            f["agent_type"] = agent_type(path)
        sc.read_file(path, f)
    sc.degraded()
    sc.fleet_rules()
    st["lines"], st["parse_errors"] = sc.lines, sc.errors
    if not dry_run:
        flush_hours(st, now, hourly_path or HOURLY_PATH)
    prune(st, now)
    if not dry_run:
        save_state(st, state_path)
    return sc.alerts


# --- report ------------------------------------------------------------------

def report(since_s, hourly_path=None, state_path=None, now=None):
    now = time.time() if now is None else now
    cut = hour_of(now - since_s)
    rows = []
    try:
        with open(hourly_path or HOURLY_PATH) as f:
            for line in f:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if r.get("hour", "") >= cut:
                    rows.append(r)
    except OSError:
        pass
    st = load_state(state_path)
    rows += [rollup_row(k, o) for k, o in st["open"].items() if k.split("|", 1)[0] >= cut]
    return rows


def parse_since(s):
    m = re.fullmatch(r"(\d+)([mhd])", s)
    if not m:
        raise SystemExit("--since must look like 30m, 1h or 2d")
    return int(m.group(1)) * {"m": 60, "h": 3600, "d": 86400}[m.group(2)]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("report")
    r.add_argument("--since", default="1h")
    r.add_argument("--json", action="store_true")
    s = sub.add_parser("scan")
    s.add_argument("--dry-run", action="store_true")
    a = ap.parse_args(argv)
    if a.cmd == "scan":
        for al in step(dry_run=a.dry_run):
            print(f"{'would alert' if a.dry_run else 'alert'}: [{al['session']}] token {al['detail']}")
        return 0
    rows = report(parse_since(a.since))
    if a.json:
        print(json.dumps(rows))
    else:
        print("hour project session agent model turns ctx_p50 ctx_p90 hit units  "
              "(units: price x (in+5out+w_read*read+1.25*c5m+2*c1h)/1e6, w_read opus 0.05 other 0.1)")
        for x in rows:
            print(x["hour"], x["project"][-24:], x["session"][:8], x["agent_type"], x["model"], x["turns"],
                  x["ctx_p50"], x["ctx_p90"], x["hit_ratio"], x["units"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
