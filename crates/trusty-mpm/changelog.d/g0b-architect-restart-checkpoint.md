Changed
- `tm architect restart` now asks the Architect for an Intent Ledger checkpoint and waits up to 90 s for the write before it restarts only the Architect's `claude`; `--force` restarts after a timeout, `--no-checkpoint` keeps the old gate. It finds the Architect by its fleet role and refuses when none is recorded. A failed stop or relaunch types nothing and reports "NOT restarted".
