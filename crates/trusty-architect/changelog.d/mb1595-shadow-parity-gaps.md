Fixed
- The Rust poller's `idle` alert now matches the deployed `fleet-poll.py`: it fires once per idle screen (the fingerprint carries a hash of the rows above the box, timers and clocks blanked) and no longer re-emits every 1800 s.
- The Rust poller applies `alert_triage.py`'s rules instead of waking for every alert, so a `ctx` alert below the 35% bar is logged, not woken; alerts the prototype sends to Haiku still wake, as in a Python dry run.
- The poll cycle raises `pm_message` from `pm-messages.jsonl` (a port of `pm_intake.py`), and a shadow reads the selftest, ctx-cycle and PM-message queues from the live inbox (`ARCHITECT_READ_INBOX_DIR`), so `selftest_fail` and `pm_message` reach the shadow.
- The input box reads `poll` when it holds only a stale poller wake line, as the deployed `input-state.py` does.
- `trusty-architect-sched parity` prints the alert and wake match percentages and a PASS or FAIL verdict (exit 1 on FAIL) against `--min-cycles` (default 120) and `--min-match-pct` (default 95); `cutover` uses the same verdict. A wake now counts only when the cycle's triage woke, not when a dry run repeats its unsent backlog.
