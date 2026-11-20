Added
- The poller runs `scripts/fleet-poll-ledger.py` once per cycle (live and shadow, never in a dry run) for the ten ledger-bound steps that stay Python until the ledger port; a missing script, a child that cannot start, a timeout (killed after 240 s), a non-zero exit or unreadable output raises a `ledger_shim` alert in the cycle's failing set.
