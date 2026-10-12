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
