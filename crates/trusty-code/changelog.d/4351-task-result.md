Added

- `task.run` and `session.status` carry an additive `result` object — `status`, `diff_ref`, `branch`, `pr_ref`, `summary` — so a caller learns where a run's change landed and whether it passed instead of only its session id (#4351).
- Every result field is nullable and the object is omitted entirely for a session that has never run, so an existing consumer sees an unchanged payload.
- A run that exhausted its turn budget reports `partial`, not `failed`.
