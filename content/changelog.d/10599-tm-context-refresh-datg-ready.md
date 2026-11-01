Changed
- `tm-context-refresh` describes the `scripts/self-clear.sh` gate as built: it runs `datg.py --project <ROOT>/local/il ready` before the dry run and again after the idle wait, only exit 0 allows a clear, and only `turn-active.py` exit 10 counts as idle (#10599).
