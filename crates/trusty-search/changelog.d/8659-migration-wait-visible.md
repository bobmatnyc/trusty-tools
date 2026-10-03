Fixed
- A schema migration queued behind a running reindex, deferred-embed pass or relocate is no longer silent. The daemon logs one INFO line naming the index, the pending migrations and the permit holder, logs the elapsed wait every 60 s, and logs the total once the permit arrives. `GET /indexes/{id}/status` reports the wait as `migration_waiting` (#8659).
