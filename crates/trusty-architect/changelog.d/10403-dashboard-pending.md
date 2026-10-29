Added
- `dashboard::pending` ports the prototype's owner-event rules (`pending`, `stale_tasks`, `last_activity`) and `dashboard::derive` gains `run_progress`, `ordered` and `goal_sessions`, all pure functions of a snapshot and `now` whose output is byte-equal to `dashboard_derive.py`; unparseable timestamps keep the prototype's result and are also listed by `skipped()` (#10403).
