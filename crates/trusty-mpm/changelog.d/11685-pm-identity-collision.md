Fixed
- `datg.py` refuses with a clear error when the pane's tmux session keeps its ledger only in `.trusty-mpm/dbatg/`, instead of writing another ledger (#11685).
- Every `datg.py` write keeps the replaced `state.json` as `<datg>/prior/state.v<N>.json` (last 20), so an overwritten version can be restored (#11685).
