# A2 parity corpus

Goldens for `tests/parity.rs` (#9171 A2). Every `*.jsonl` file here and
`corpus.json` are written by `regen.py`, which runs the Architect's live
prototype Python. Do not edit them by hand, and never regenerate them from the
Rust.

Parity baseline: the live prototype in `<prototype-dir>`, the Architect
project's `scripts/` directory. The classifier is the copy inline in
`fleet-poll.py`, the poller the live host runs; `fleet-classify.py` is a stale
split of it (45/50 ctx bar, footer-only agent rows, no `current_waiting`,
`turn_active`, `account` or `rc_failed`) and is not used. `regen.py` also
loads `input-state.py`, `quiet-sessions.py` and `self-ctx.py`, the modules
`fleet-poll.py` itself loads. It reads that directory and writes nothing there
(`-B`, `-I` and `sys.dont_write_bytecode`; importing `fleet-poll.py` opens no
file for writing).

## Regenerate

From the repository root, with `<prototype-dir>` as the one argument:

```bash
python3 -B -I crates/trusty-architect/tests/fixtures/parity/regen.py <prototype-dir>
git diff --exit-code crates/trusty-architect/tests/fixtures/parity
```

A clean `git diff` means the Rust goldens still match the prototype.

## Files

| File | Python call | `json.dumps` form |
|---|---|---|
| `corpus.json` | inputs: captures, scenarios, file specs | `indent=1` |
| `input_state.jsonl` | `input_state.classify(capture)` | default |
| `parse_pane.jsonl` | `fleet_poll.parse_pane(capture)` | default |
| `evaluate.jsonl` | `evaluate(info, ps, now, "tm-x", "%1")` per step, with `ps` | `ensure_ascii=False`, as `alerts.jsonl` rows |
| `dedup.jsonl` | `dedup(state, alerts, now)` per step, with `state` | default, as `poll-state.json` |
| `plan.jsonl` | `fleet_plan_alerts(infos, now)` | `ensure_ascii=False` |
| `clean.jsonl` | `[text, clean(text), fp(text)]` | `ensure_ascii=False` |
| `quiet.jsonl` | `sorted(read_quiet_sessions(path))`, `is_test_fixture_session(name)` | default |
| `self_ctx.jsonl` | `newest_transcript`, `tail_last_assistant_usage`, `self_ctx_tokens` | `sort_keys=True` (the Rust usage map is ordered) |

Captures come from the prototype tests' inline strings and parametrize cases
(`tests/test_fleet_poll.py`'s `screen()` helper is copied into `regen.py`;
`scripts/test_fleet_poll_idle.py` and `scripts/test_fleet_poll_rc.py` supply
the agent-row, waiting, `account` and `rc_failed` shapes), plus
`../idle-pane-labeled-rule.txt`, the scrubbed copy of the prototype's one
real capture (taken from `python/trusty-architect/tests/fixtures/`). Real
captured panes replace or extend these in A3.
