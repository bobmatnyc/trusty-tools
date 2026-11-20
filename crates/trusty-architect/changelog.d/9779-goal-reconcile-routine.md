Added
- `poller::routines`: a `goals_dir` routine (goal_reconcile) raises `routine_stale` per active goal doc whose `reconciled_at` is missing, unreadable or older than the routine's stale threshold; an unreadable goal doc or folder is flagged too. A routine can set `stale_after_s` to override the default of twice `interval_s`; unknown keys are refused (#9779).
