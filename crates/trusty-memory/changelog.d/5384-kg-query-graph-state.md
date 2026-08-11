Fixed

- `kg_query` no longer reports `graph_state: "graph_empty"` when the whole-graph triple count could not be read ([#5384](https://github.com/bobmatnyc/trusty-tools/issues/5384))
  - The count failed open to `0` in `trusty-common`, and #4775's classifier reads `0` as an empty graph — so a redb read failure produced the exact false claim #4775 exists to prevent. `kg_query` now returns the error.
  - `GET /api/v1/palaces/{id}/kg/count` answers 500 instead of `{"active": 0}`, and `kg_graph`'s `truncated` flag no longer computes against a `0` that would make every payload look complete.
  - The status, console-metrics, and palace-info roll-ups still degrade to `0` — they have no field for "unknown", per #4637 — but do it at a single named call site (`kg_triple_count_or_zero`) that logs the palace and the error. The chat prompt prints `unknown (read failed)` rather than a `0` the model would repeat.
