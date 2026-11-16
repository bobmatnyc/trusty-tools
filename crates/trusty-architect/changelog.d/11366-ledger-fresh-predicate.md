Added
- `ledger::fresh` ports the `datg.py fresh` and `datg.py ready` verdicts (exit 0, 1 or 2) so `tm architect restart` can gate on a fresh Intent Ledger checkpoint; an unreadable ledger is never fresh (#11366).
