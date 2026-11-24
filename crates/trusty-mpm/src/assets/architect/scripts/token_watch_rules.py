"""The pure half of the fleet token watcher, the token_watch entry point (#10571 S1).

#10570: no sibling is named with its file extension here: pm-guard reads such a name as a token file.
Why: the token_watch entry point passed the 500-SLOC cap, so its rule engine lives
here and its transcript, state-file and rollup I/O stays in the entry point.
What: the DEFAULTS thresholds, COOLDOWN, the price proxy (turn_units), hour
keys, the per-turn rules (ScanRules.turn), the fleet rules (fleet_rules,
degraded), alert dedup (emit), the hourly rollup row and state pruning. Every
method works on the state dict and the parsed line it is given; none opens a
file. The entry point's Scan subclasses ScanRules and adds read_file, and the
entry point re-exports every name here.
Test: tests/test_token_watch.py, tests/test_ctx_bar.py
"""
import importlib.util
import os
import re
import statistics
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))


def _load_ctx_bar():
    spec = importlib.util.spec_from_file_location("ctx_bar", os.path.join(HERE, "ctx_bar.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ctx_bar = _load_ctx_bar()

DEFAULTS = {
    "ctx_main": [200_000, 315_000, 350_000, 400_000],
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
PRICE = {"opus": 5.0, "sonnet": 3.0, "haiku": 1.0}
# Families recognised but with no price in this file. Their units are 0.0 and rollup rows carry
# units_unknown=true; a price here is the owner's/an engineer's to add, never inferred (ar-Te4).
UNPRICED = ("fable",)
POLL_RE = re.compile(r"^\s*tm wait\b|\bsleep \d+")
# The home-directory prefix of a Claude Code project slug (macOS or Linux home).
SESSION_DIR_RE = re.compile(r"^-(?:Users|home)-[^-]+-")


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
    return next((k for k in (*PRICE, *UNPRICED) if k in m), "sonnet")


def turn_units(model, u):
    """Spec proxy: price x (in + 5 out + w_read x read + 1.25 c5m + 2 c1h) / 1e6."""
    fam = family(model)
    if fam in UNPRICED:
        return 0.0
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


# --- engine ------------------------------------------------------------------

class ScanRules:
    """The rule engine over one scan's state; the entry point's Scan adds the file reads."""

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
            if f.get("off0") and not f.get("triage_gate"):
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
        k = "|".join((h, f["project"], f["session"], atype, model, effort_of(d)))
        o = self.st["open"].setdefault(k, {"turns": 0, "think": 0, "ctx": [], "out": 0, "rd": 0, "cc": 0, "in": 0,
                                           "rw_exp": 0, "rw_unexp": 0, "polls": 0, "units": 0.0})
        o["turns"] += 1
        o["ctx"].append(ctx)
        o["out"] += out_t
        o["think"] = o.get("think", 0) + thinking_of(u)
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
        buckets = ctx_bar.main_buckets(self.label(f), th[rule]) if rule == "ctx_main" else th[rule]
        crossed = [b for b in buckets if ctx >= b]
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


def effort_of(d):
    """The line's top-level effort, "unknown" when absent (ar-T94@1120)."""
    e = d.get("effort")
    return e.replace("|", "/") if isinstance(e, str) and e else "unknown"


def thinking_of(u):
    """usage.output_tokens_details.thinking_tokens (inside output_tokens); 0 when absent."""
    det = u.get("output_tokens_details")
    t = det.get("thinking_tokens") if isinstance(det, dict) else None
    return t if isinstance(t, int) else 0


def split_open_key(key):
    """(hour, project, session, agent_type, model, effort); a pre-effort 5-part key reads as "unknown"."""
    p = key.split("|", 5)
    return tuple(p) if len(p) == 6 else tuple(p) + ("unknown",)


def rollup_row(key, o):
    h, project, session, atype, model, effort = split_open_key(key)
    tot = o["rd"] + o["cc"] + o["in"]
    return {"hour": h, "project": project, "session": session, "agent_type": atype, "model": model,
            "effort": effort, "thinking_tokens": o.get("think", 0), "turns": o["turns"], "ctx_p50": pct(o["ctx"], 0.5), "ctx_p90": pct(o["ctx"], 0.9),
            "out_tokens": o["out"], "hit_ratio": round(o["rd"] / tot, 4) if tot else None,
            "rewrites_expected": o["rw_exp"], "rewrites_unexpected": o["rw_unexp"],
            "poll_turns": o["polls"], "units": round(o["units"], 3),
            "units_opus": round(o["units"], 3) if family(model) == "opus" else 0.0,
            "units_unknown": family(model) in UNPRICED}


def prune(st, now):
    st["seen"] = st["seen"][-5000:]
    st["files"] = {p: f for p, f in st["files"].items() if now - f.get("seen_at", now) < 86400}
    st["fired"] = {k: t for k, t in st["fired"].items() if now - t < FIRED_TTL}
    st["fleet"] = {h: v for h, v in st["fleet"].items() if h >= hour_of(now - 8 * 86400)}
    st["miss_count"] = {k: v for k, v in st["miss_count"].items() if k.rsplit("|", 1)[1] >= hour_of(now - 7200)}


def parse_since(s):
    m = re.fullmatch(r"(\d+)([mhd])", s)
    if not m:
        raise SystemExit("--since must look like 30m, 1h or 2d")
    return int(m.group(1)) * {"m": 60, "h": 3600, "d": 86400}[m.group(2)]
