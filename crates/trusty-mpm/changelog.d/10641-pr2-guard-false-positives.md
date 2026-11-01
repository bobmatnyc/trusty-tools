Fixed
- pm-guard reads a Python script body as Python, so the Architect's seeded scripts run; a body naming a credential call or secret file still refuses (#10570, #10598).
- pm-guard's nested-run scan no longer reads shell comment prose as a run, and a `#` inside a quote or after a line continuation stays code (#10598, #10548).
- `token-hourly.jsonl`-style usage tallies no longer count as token files (#10570).
- The seeded `self-clear.sh` runs only the scripts beside it, and no longer calls `turn-active.py`, which was never seeded (#10570).
