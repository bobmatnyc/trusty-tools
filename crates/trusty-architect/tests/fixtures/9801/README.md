# #9801 slice 1 parity corpus

Goldens for `tests/parity_usage.rs`: the usage step (`usage.py`'s poller
path and `fleet-poll.py`'s `usage_alerts` wrapper) and `notify_macos`.
Every file here except this README, `regen_9801.py` and
`python_tests_9801.txt` is written by `regen_9801.py`, which runs the
Architect prototype's Python. Do not edit them by hand, and never regenerate
them from the Rust.

Baseline: the prototype's `scripts/` at Architect commit 0d1ef3b. `usage.py`
is unchanged since 76a5a12 and `notify_macos` since 7892d0f.

`regen_9801.py` loads `usage.py` and `fleet-poll.py` read-only (`-B`, `-I`,
`sys.dont_write_bytecode`). It points the ledger, statusline cache,
claude.json, inbox and `poll.log` at a system tempdir before the import. The
probe launcher is always a fake, so no `usage-probe.sh` runs and no pane is
touched. `subprocess.run` is replaced while `notify_macos` runs, so no
notification is sent.

## Regenerate

From the repository root, with `<prototype-dir>` the prototype's `scripts/`:

```bash
python3 -B -I crates/trusty-architect/tests/fixtures/9801/regen_9801.py <prototype-dir>
git diff --exit-code crates/trusty-architect/tests/fixtures/9801
```

## Files

| File | Python call | `json.dumps` form |
|---|---|---|
| `corpus_9801.json` | inputs | `indent=1` |
| `usage_alerts.jsonl` | `usage.usage_alerts(rows, now, current)` | `ensure_ascii=False` |
| `plan.jsonl` | `usage.plan(rows, now, current)`, with its `runway` | `ensure_ascii=False` |
| `usage_rows.jsonl` | `fleet_poll.usage_alerts(...)`: the alert rows and the `poll.log` lines | `ensure_ascii=False` |
| `effective.jsonl` | `usage.effective(rec, now)` | default |
| `project.jsonl` | `usage.project(rows, account, now, kind)` | default |
| `cache.jsonl` | `usage.read_cache(now)` over a file with a set mtime | `ensure_ascii=False` |
| `poll_step.jsonl` | `usage.poll_step(...)` per step: state, probe launches, alerts, ledger bytes | `ensure_ascii=False, sort_keys=True` |
| `notify.jsonl` | `fleet_poll.notify_macos(title, message)`: the argv and keyword arguments | `ensure_ascii=False` |
| `rulings.jsonl` | for each ruled case, what the unpatched prototype does | `ensure_ascii=False, sort_keys=True` |
| `python_tests_9801.txt` | hand-written: the `test_usage.py` tests ported by name, and the ones left out | |

A golden `{"raises": "<class>"}` row requires the Rust to return an error;
the Python class name is not compared.

## Divergences from the prototype

Each one is a named ruling in `rulings.jsonl`;
`every_divergence_from_the_prototype_is_named` fails on a new one.

- `burn_rate_empty_run_is_no_rate` (applied in `regen_9801.py` as the one
  patch): the prototype's `burn_rate` raises `IndexError` when an account's
  newest sample is older than the burn span, which makes `usage_alerts` raise
  whenever any account's newest reading is over 48 h old or the current
  account's is over 3 h old. The port returns no rate, as the function's own
  docstring states. Pending an Architect ruling.
- `failed_launch_is_not_a_probe` (Fail-Open Check): the prototype records
  `last_probe` and `tries` before `usage-probe.sh` starts, so a launch that
  fails counts as a probe, and the raise loses that cycle's usage alerts. The
  port records the probe only after a successful launch, logs `usage probe
  ERROR`, and keeps the alerts. Test: `tests/steps_fail_open.rs`.
- `notify_failure_is_logged`: the prototype logs a spawn failure or timeout
  and ignores a non-zero `osascript` exit. The port logs all three as `notify
  ERROR`; none changes poller state, and none is retried.
