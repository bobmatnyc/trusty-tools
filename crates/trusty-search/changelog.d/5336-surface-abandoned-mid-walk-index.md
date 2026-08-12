Added

- `GET /health` reports `indexes_stuck_mid_walk` and `GET /indexes/:id/status` reports `stuck_mid_walk`: an index whose lexical walk started and was then abandoned (the reindex task panicked or was cancelled, leaving the stage frozen at `in_progress`) is now distinguishable from one that is genuinely mid-reindex, and forces `status: "degraded"`. Detection only — clear it with `POST /indexes/:id/reindex` ([#5336](https://github.com/bobmatnyc/trusty-tools/issues/5336))
