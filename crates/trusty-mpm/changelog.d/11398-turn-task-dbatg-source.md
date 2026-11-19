Fixed
- `tm hook --turn-task` now reads the dbatg ledger (`.trusty-mpm/dbatg/<session>/state.json`, `session.task` beginning with an open task id) before the datg store, so a live PM turn shows its Task id. A dbatg store without a task id shows nothing and never falls back to an older datg `session.current` (#11398).
