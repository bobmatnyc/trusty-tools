"""token_watch.py: one test per rule on fixture transcripts (synthetic, no real
transcripts). Each rule test also runs a below-threshold control, which is the
fail-first proof: remove the rule and the positive assertion fails; loosen it
and the control fails."""
import importlib.util
import json
import os
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S1: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("token_watch", os.path.join(SCRIPTS, "token_watch.py"))
tw = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tw)
# #10571 S1: the poller-hook check (fleet-poll.py token_alerts, REEMIT) ports with the poller in S3.
# A Claude Code project slug starts with the home directory; built here so no home path is committed.
HOME_SLUG = "-" + "Users" + "-x-"

NOW = datetime(2026, 10, 7, 12, 30, 0, tzinfo=timezone.utc).timestamp()
OPUS, SONNET = "claude-opus-5", "claude-sonnet-5"


def iso(t):
    return datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z")


def line(mid, t, model=OPUS, read=0, create=0, inp=0, out=10, cmd=None):
    content = [{"type": "tool_use", "name": "Bash", "input": {"command": cmd}}] if cmd else \
        [{"type": "text", "text": "x"}]
    return json.dumps({"type": "assistant", "timestamp": iso(t), "message": {
        "id": mid, "model": model, "content": content,
        "usage": {"input_tokens": inp, "cache_read_input_tokens": read,
                  "cache_creation_input_tokens": create, "output_tokens": out}}}) + "\n"


class Env:
    def __init__(self, tmp):
        self.base = tmp / "projects"
        self.tmp = tmp

    def main(self, sid="aaaaaaaa-0000", proj=HOME_SLUG + "proj"):
        p = self.base / proj / f"{sid}.jsonl"
        p.parent.mkdir(parents=True, exist_ok=True)
        return p

    def sub(self, aid, sid="aaaaaaaa-0000", proj=HOME_SLUG + "proj", atype="research"):
        p = self.base / proj / sid / "subagents" / f"agent-{aid}.jsonl"
        p.parent.mkdir(parents=True, exist_ok=True)
        (p.parent / f"agent-{aid}.meta.json").write_text(json.dumps({"agentType": atype}))
        return p

    def write(self, path, *lines, mode="w"):
        with open(path, mode) as f:
            f.write("".join(lines))
        os.utime(path, (NOW, NOW))

    def run(self, now=NOW, dry_run=False):
        return tw.step(now=now, dry_run=dry_run, base=str(self.base),
                       state_path=str(self.tmp / "state.json"),
                       hourly_path=str(self.tmp / "hourly.jsonl"),
                       thresholds_path=str(self.tmp / "none.json"), names={})

    def state(self):
        return tw.load_state(str(self.tmp / "state.json"))


def fps(alerts):
    return sorted(a["fingerprint"] for a in alerts)


def test_ctx_main_buckets_once(tmp_path):
    e = Env(tmp_path)
    e.write(e.main(), *[line(f"m{i}", NOW - 600 + i, read=c) for i, c in
                        enumerate([190_000, 205_000, 230_000, 320_000])])
    assert fps(e.run()) == ["ctx_main:200k", "ctx_main:315k"]
    assert e.run() == []                      # incremental: nothing new, nothing raised
    e.write(e.main(), line("m9", NOW - 10, read=190_000), mode="a")
    assert e.run() == []                      # below every bucket: control


def test_ctx_sub_uses_subagent_buckets(tmp_path):
    e = Env(tmp_path)
    e.write(e.sub("bbbbbbbb1"), line("s1", NOW - 60, read=160_000))
    e.write(e.main(), line("m1", NOW - 60, read=160_000))   # main at 160k: no main bucket
    assert fps(e.run()) == ["ctx_sub:150k"]


def test_miss_idle_vs_unexpected(tmp_path):
    e = Env(tmp_path)
    # 7-minute gap (> 300 s subagent TTL), 90% rewrite, 160k Opus -> expected -> miss_idle
    e.write(e.sub("idleidle1"), line("a1", NOW - 900, read=10_000, create=0, out=5),
            line("a2", NOW - 480, read=16_000, create=160_000))
    # same rewrite after a 2-minute gap -> unexpected -> miss_unexpected
    e.write(e.sub("quickqui1"), line("b1", NOW - 900, read=10_000, create=0, out=5),
            line("b2", NOW - 780, read=16_000, create=160_000))
    got = fps(e.run())
    assert "miss_idle:idleidle" in got and "miss_unexpected:b2" in got
    assert "miss_idle:quickqui" not in got and "miss_unexpected:a2" not in got
    o = {k: v for k, v in e.state()["open"].items()}
    assert sum(v["rw_exp"] for v in o.values()) == 1 and sum(v["rw_unexp"] for v in o.values()) == 1


def test_miss_unexpected_burst_is_capped(tmp_path):
    e = Env(tmp_path)
    ls = [line("c0", NOW - 900, read=10_000)]
    ls += [line(f"c{i}", NOW - 900 + 20 * i, read=10_000, create=150_000) for i in range(1, 6)]
    e.write(e.main(), *ls)
    got = fps(e.run())
    assert len([g for g in got if g.startswith("miss_unexpected:c")]) == 3
    assert [g for g in got if "burst" in g] == [f"miss_unexpected:burst:{tw.hour_fp(NOW - 800)}"]


def test_poll_agent_fires_at_twelve(tmp_path):
    e = Env(tmp_path)
    mk = lambda n: [line(f"p{i}", NOW - 1500 + 60 * i, model=SONNET, read=1000, cmd="tm wait --for run x")
                    for i in range(n)]
    e.write(e.sub("pollpoll1"), *mk(11))
    assert e.run() == []                      # 11 polls: control
    e2 = Env(tmp_path / "again")
    e2.tmp.mkdir()
    e2.write(e2.sub("pollpoll1"), *mk(12))
    assert fps(e2.run()) == ["poll_agent:pollpoll"]
    assert e2.run() == []


def test_hit_low(tmp_path):
    e = Env(tmp_path)
    mk = lambda r: [line(f"h{i}", NOW - 1000 + i, read=r, create=0, inp=100_000 - r) for i in range(20)]
    e.write(e.main(), *mk(98_000))
    assert e.run() == []                      # 98% hit: control
    e2 = Env(tmp_path / "low")
    e2.tmp.mkdir()
    e2.write(e2.main(), *mk(80_000))
    got = e2.run()
    assert fps(got) == [f"hit_low:main:{tw.hour_fp(NOW - 981)}"]
    assert got[0]["detail"].startswith("hit_low: hit 0.80")


def _fleet_state(tmp, **fleet):
    st = tw.new_state()
    st["fleet"] = fleet.pop("fleet", {})
    st["starts"] = fleet.pop("starts", {})
    tw.save_state(st, str(tmp / "state.json"))


def _hours(n, **vals):
    return {tw.hour_of(NOW - 3600 * (n - i)): dict(vals) for i in range(n)}


def test_opus_share(tmp_path):
    e = Env(tmp_path)
    _fleet_state(tmp_path, fleet={tw.hour_of(NOW): {"units": 100.0, "opus": 85.0, "polls": 0}})
    assert [a["rule"] for a in e.run()] == ["opus_share"]
    _fleet_state(tmp_path, fleet={tw.hour_of(NOW): {"units": 100.0, "opus": 70.0, "polls": 0}})
    assert e.run() == []


def test_poll_rate_and_burn(tmp_path):
    e = Env(tmp_path)
    fleet = _hours(30, units=100.0, opus=0.0, polls=40)
    last = tw.hour_of(NOW - 3600)
    fleet[last]["polls"] = 200
    _fleet_state(tmp_path, fleet=fleet)
    assert fps(e.run()) == [f"poll_rate:{tw.hour_fp(NOW - 3600)}"]
    fleet = _hours(30, units=100.0, opus=0.0, polls=0)
    for h in (tw.hour_of(NOW - 3600), tw.hour_of(NOW - 7200)):
        fleet[h]["units"] = 400.0
    _fleet_state(tmp_path, fleet=fleet)
    assert fps(e.run()) == [f"burn:{tw.hour_fp(NOW - 3600)}"]
    fleet[tw.hour_of(NOW - 7200)]["units"] = 100.0     # only one hot hour: control
    _fleet_state(tmp_path, fleet=fleet)
    assert e.run() == []


def test_session_start_churn(tmp_path):
    e = Env(tmp_path)
    mk = lambda n: {HOME_SLUG + "busy": [[NOW - 60 * i, f"sid{i}"] for i in range(n)]}
    _fleet_state(tmp_path, starts=mk(6))
    assert e.run() == []                      # 6 starts: control
    _fleet_state(tmp_path, starts=mk(7))
    got = e.run()
    assert [a["rule"] for a in got] == ["session_start_churn"] and "7 main-session starts" in got[0]["detail"]


def _user(t, text, entry):
    return json.dumps({"type": "user", "timestamp": iso(t), "entrypoint": entry,
                       "message": {"role": "user", "content": text}}) + "\n"


def test_session_dir_re_strips_a_macos_or_linux_home_slug():
    """#10571 S1: the slug prefix pattern names no home path and also strips a Linux home."""
    assert tw.SESSION_DIR_RE.sub("", HOME_SLUG + "busy") == "busy"
    assert tw.SESSION_DIR_RE.sub("", "-home-x-busy") == "busy"
    assert tw.SESSION_DIR_RE.sub("", "-private-var-T") == "-private-var-T"


def test_session_start_churn_skips_triage_gate(tmp_path):
    e = Env(tmp_path)
    gate, other = "-private-var-T", HOME_SLUG + "busy"
    for i in range(8):
        e.write(e.main(f"g{i}", gate), _user(NOW - 60 * i, tw.TRIAGE_PREFIX + " ...", "sdk-cli"),
                line(f"mg{i}", NOW - 60 * i + 1))
        e.write(e.main(f"n{i}", other), _user(NOW - 60 * i, "hello", "cli"),
                line(f"mn{i}", NOW - 60 * i + 1))
    got = e.run()
    assert [a["rule"] for a in got] == ["session_start_churn"]
    assert "busy" in got[0]["detail"] and "private" not in got[0]["detail"]
    # same prefix from a different entrypoint is not exempt
    e2 = Env(tmp_path / "b")
    (tmp_path / "b").mkdir()
    for i in range(8):
        e2.write(e2.main(f"g{i}", gate), _user(NOW - 60 * i, tw.TRIAGE_PREFIX, "cli"),
                 line(f"mg{i}", NOW - 60 * i + 1))
    assert [a["rule"] for a in e2.run()] == ["session_start_churn"]


def test_fanout(tmp_path):
    e = Env(tmp_path)
    for i in range(3):
        e.write(e.sub(f"fan0000{i}"), line(f"f{i}", NOW - 30, read=1000))
    assert e.run() == []                      # 3 concurrent = cap: control
    e.write(e.sub("fan00009"), line("f9", NOW - 20, read=1000))
    assert fps(e.run()) == ["fanout:4"]


def test_hourly_rollup_idempotent(tmp_path):
    e = Env(tmp_path)
    e.write(e.main(), *[line(f"r{i}", NOW - 600 + i, model=OPUS, read=100_000, out=50) for i in range(4)])
    later = NOW + 1900                        # next hour: the open hour is flushed
    e.run(now=later)
    rows = [json.loads(x) for x in open(tmp_path / "hourly.jsonl")]
    assert len(rows) == 1 and rows[0]["turns"] == 4 and rows[0]["agent_type"] == "main"
    assert rows[0]["units_opus"] == rows[0]["units"] > 0
    (tmp_path / "state.json").unlink()        # lose state: same files, same hour
    e.run(now=later)
    assert len(open(tmp_path / "hourly.jsonl").read().splitlines()) == 1


def test_truncated_last_line_is_picked_up_later(tmp_path):
    e = Env(tmp_path)
    p = e.main()
    whole = line("t2", NOW - 20, read=320_000)
    e.write(p, line("t1", NOW - 30, read=1000), whole[:40])
    assert e.run() == []                      # no crash, partial line not parsed
    e.write(p, whole[40:], mode="a")
    assert fps(e.run()) == ["ctx_main:315k"]
    assert sum(o["turns"] for o in e.state()["open"].values()) == 2


def test_duplicate_message_id_counts_once(tmp_path):
    e = Env(tmp_path)
    e.write(e.main(), *[line("same", NOW - 60, read=10_000)] * 3)
    e.run()
    assert sum(o["turns"] for o in e.state()["open"].values()) == 1


def test_dry_run_writes_nothing(tmp_path):
    e = Env(tmp_path)
    e.write(e.main(), line("d1", NOW - 60, read=320_000))
    assert fps(e.run(dry_run=True)) == ["ctx_main:315k"]
    assert not (tmp_path / "state.json").exists() and not (tmp_path / "hourly.jsonl").exists()


def test_watcher_degraded_on_bad_lines(tmp_path):
    e = Env(tmp_path)
    p = e.main()
    e.write(p, *["not json\n"] * 5, *[line(f"g{i}", NOW - 100 + i, read=1000) for i in range(20)])
    assert [a["rule"] for a in e.run()] == ["watcher_degraded"]


def test_no_message_text_in_alerts_or_state(tmp_path):
    e = Env(tmp_path)
    secret = "SECRET-PROMPT-TEXT"
    d = json.loads(line("z1", NOW - 60, read=260_000))
    d["message"]["content"] = [{"type": "text", "text": secret}]
    e.write(e.main(), json.dumps(d) + "\n")
    got = e.run()
    assert got and secret not in json.dumps(got) and secret not in (tmp_path / "state.json").read_text()


def _stopped(mid, t, stop="end_turn"):
    d = json.loads(line(mid, t, read=1000))
    d["message"]["stop_reason"] = stop
    return json.dumps(d) + "\n"


def test_fanout_counts_live_subagents_only(tmp_path):
    """4 subagents started, 2 finished (final end_turn): 2 live, under the cap."""
    e = Env(tmp_path)
    for i in range(2):
        e.write(e.sub(f"live000{i}"), line(f"l{i}", NOW - 30, read=1000))
    for i in range(2):
        e.write(e.sub(f"done000{i}"), line(f"d{i}a", NOW - 60, read=1000),
                _stopped(f"d{i}b", NOW - 20))
    assert e.run() == []
    # a finished subagent resumed by a later turn counts live again
    e.write(e.sub("done0000"), line("d0c", NOW - 5, read=1000), mode="a")
    e.write(e.sub("live0009"), line("l9", NOW - 5, read=1000))
    assert fps(e.run()) == ["fanout:4"]


def test_family_fable_is_not_priced_as_sonnet():
    # ar-Te4: family() had no fable entry, so Fable turns fell back to sonnet and were priced as Sonnet.
    u = {"input_tokens": 10, "output_tokens": 1000, "cache_read_input_tokens": 50_000}
    assert tw.family("claude-fable-5-2") == "fable"
    assert tw.turn_units("claude-fable-5-2", u) == 0.0           # no price in the file: unknown, not Sonnet's
    assert tw.turn_units("claude-sonnet-5-5", u) > 0             # control: a priced family still counts
    assert tw.family("claude-unheard-9") == "sonnet"             # unknown models keep the old fallback


def test_rollup_row_flags_fable_cost_unknown():
    o = {"turns": 1, "ctx": [100], "out": 5, "rd": 0, "cc": 0, "in": 100, "rw_exp": 0, "rw_unexp": 0,
         "polls": 0, "units": 0.0}
    assert tw.rollup_row("h|p|s|main|claude-fable-5-2", o)["units_unknown"] is True
    assert tw.rollup_row("h|p|s|main|claude-opus-5-5", o)["units_unknown"] is False


def test_effort_and_thinking_in_rollup_and_legacy_key():
    assert tw.effort_of({"effort": "medium"}) == "medium" and tw.effort_of({}) == "unknown"
    assert tw.thinking_of({"output_tokens_details": {"thinking_tokens": 9}}) == 9 and tw.thinking_of({}) == 0
    o = {"turns": 1, "ctx": [1], "out": 1, "rd": 0, "cc": 0, "in": 1, "rw_exp": 0, "rw_unexp": 0,
         "polls": 0, "units": 0.0, "think": 4}
    r = tw.rollup_row("h|p|s|main|claude-opus-5-5|low", o)
    assert (r["effort"], r["thinking_tokens"]) == ("low", 4)
    legacy = tw.rollup_row("h|p|s|main|claude-opus-5-5", {k: v for k, v in o.items() if k != "think"})
    assert (legacy["effort"], legacy["thinking_tokens"]) == ("unknown", 0)
