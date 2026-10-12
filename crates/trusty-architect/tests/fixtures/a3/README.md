# A3 parity corpus

Goldens for `tests/parity_a3.rs`, `tests/parity_wake.rs` and the Unicode `\d` table (#9171 A3a, A3b-1).
Every file here except this README and `regen_a3.py`, and
`src/decimal_table.rs`, is written by `regen_a3.py`, which runs the frozen
prototype `fleet-poll.py` (Architect commit 988152c). Do not edit them by hand,
and never regenerate them from the Rust.

`regen_a3.py` imports the prototype read-only (`-B`, `-I`,
`sys.dont_write_bytecode`), points `SUPERVISOR_INBOX_DIR`, `poll.log` and
`events.jsonl` at a system tempdir before any call, replaces the clock with a
fixed `now`, and injects the host probe values. It never touches the live
inbox, `poll-state.json` or `alerts.jsonl`.

## Regenerate

From the repository root, with `<prototype-dir>` the Architect prototype's
`scripts/` directory:

```bash
python3 -B -I crates/trusty-architect/tests/fixtures/a3/regen_a3.py <prototype-dir>
git diff --exit-code crates/trusty-architect/tests/fixtures/a3 crates/trusty-architect/src/decimal_table.rs
```

## Files

| File | Python call | `json.dumps` form |
|---|---|---|
| `corpus_a3.json` | inputs | `indent=1` |
| `list_panes.jsonl` | `list_panes()` over scripted `tmux` stdout | default |
| `band.jsonl` | `builder_band(load15, cores, free, mem, swap, prev)` | default |
| `band_alerts.jsonl` | `band_alerts(state, now, ...)` per step | `ensure_ascii=False` |
| `host.jsonl` | `host_alerts(now, state)` with injected probe values | `ensure_ascii=False` |
| `events.jsonl` | `read_events(state)` per file step | `ensure_ascii=False` |
| `self_ctx_alerts.jsonl` | `self_ctx_alerts(now)`, plus whether it logged | `ensure_ascii=False` |
| `transcript_ctx.jsonl` | `transcript_ctx_pct(pane)` per query | default |
| `alerts_lines.jsonl` | the append in `cycle()`: `json.dumps(a, ensure_ascii=False) + "\n"` | bytes |
| `digits.jsonl` | every code point `re.compile(r"\d")` matches, as ranges | default |
| `wake.jsonl` | `wake(n, panes, dry_run, logged)` over scripted `tmux`: argv log and phrase (or the raise) | `ensure_ascii=False` |
| `wake_rulings.jsonl` | for each ruled scenario, what the prototype does instead | `ensure_ascii=False` |
| `dispatch_wake.jsonl` | `dispatch_wake(state, [], new, dry_run, logged)` per step, with a four-argument fake `wake` | `sort_keys=True` |
| `python_tests_a3a.txt`, `python_tests_a3b.txt` | hand-written: the prototype tests each phase ports by name | |

## Wake divergences (#9171 A3b-1)

`wake.jsonl` holds what the port must do. Where an Architect ruling changes
the prototype's behaviour, it holds the ruled argv and phrase, and
`wake_rulings.jsonl` records the prototype's own:

- A3b Q1: an empty box after typing sends nothing (the prototype sends
  `BSpace` x `len(msg)` into the empty box). Scenario
  `empty_box_after_typing`; the ported test
  `test_wake_cleans_up_its_own_text_when_confirmation_fails` is rewritten to
  match.
- A3 Q3, rule 4: Enter only when the normalised box text equals the message;
  `BSpace` x the typed character count only when the box ends with it,
  otherwise nothing (the prototype submits on a substring match). Scenarios
  `message_plus_extra_text`, `draft_then_message_*`.

One more difference is normalised in `tests/parity_wake.rs`, not ruled: the
port types `send-keys -t P -l -- MSG` (A3a plan finding 4); the prototype
types `-l MSG`. The replay drops that `--` and nothing else.

Two prototype tests are stale against the live script and are rewritten
under their own names: `test_dispatch_wake_carries_pending_*` call a
three-argument fake `wake` that the live four-argument `dispatch_wake` call
raises `TypeError` on. `dispatch_wake.jsonl` uses a four-argument fake.

The Python `re` `\d` set depends on the Unicode version of the Python that
runs the live poller (`/opt/homebrew/bin/python3`, 3.14 / Unicode 16.0 at
generation); `src/decimal_table.rs` records both.

## Cycle and loop divergences (#9171 A3b-2)

- Ruling Q2: `read_events` holds its offset at the last newline. Case
  `truncated_last_line_then_completed` in `events.jsonl` keeps the
  prototype's rows (offset 115, the fragment lost); `tests/parity_a3.rs`
  compares every other row byte for byte and checks the ruled rows (offset
  95, then both events) on their own.
- Ruling Q4: a bad `LOAD_FACTOR` turns off host alerts only; the prototype's
  `host_alerts` raises and the whole cycle is lost. No golden covers it.
- `test_quiet_sessions_file_missing_means_nothing_is_quiet` is stale: its
  default `screen(PM_TURN)` carries `ctx 30%`, over the live 22% bar, so
  the live script writes `idle` and `ctx`, not `["idle"]`. The port keeps
  the input and asserts the live kinds.

## Prototype tests stale against the live script

Run 2026-10-12 against `fleet-poll.py` at 988152c (`pytest -k` over the A3a
list, inbox in a scratch dir): 17 of the A3a cases fail. Eight are in the
A3a reconcile list (the seven classify cases on the old 45/50 ctx bar, and
`test_events_become_alerts_except_idle_prompt`). The other six, each ported
with its input kept and the live result asserted:

| Test | Why it fails live |
|---|---|
| `test_status_line_ctx_wins_over_the_transcript` | status `ctx 30%` now alerts (22% bar); the test expects none |
| `test_status_line_without_a_plain_ctx_percent_uses_the_transcript` (2 params) | picks the transcript by cwd; the live script resolves it by the pane's session id |
| `test_transcript_ctx_is_cached_until_the_file_changes` | calls `transcript_ctx_pct('/w/proj')` with a cwd; live takes a pane id and returns `None` |
| `test_a_sustained_5min_average_above_1_5x_cores_alerts` | the load alert reads the 15-minute average, not the 5-minute one |
| `test_load_factor_is_env_overridable` | same: its 15-minute 5.0 is under either line |
| `test_band_alert_fires_only_on_change_and_first_run` | the band detail names `load15`, not `load5` |
