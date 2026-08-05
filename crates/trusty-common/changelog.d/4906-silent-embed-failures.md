Fixed

- deferred embedding no longer drops failures silently — a drawer can no longer be stored, durable, and permanently unfindable (closes [#4906](https://github.com/bobmatnyc/trusty-tools/issues/4906))
  - the background embed lane retries transient failures with bounded exponential backoff instead of giving up on the first error
  - a final failure writes a durable row to the palace's `embed_failures.json` ledger, so the loss outlives the `warn!` that used to be its only trace
  - "no embedder on this host" is separated from "the embedder is here and this drawer failed"; only the second marks a drawer, so a machine with no model downloaded is not reported as having thousands of broken drawers
  - new `PalaceHandle::embed_health()` answers "which drawers have no vector" by set-differencing the drawer table against the vector index, replacing a self-retrieval guess
  - new `PalaceHandle::backfill_missing_vectors()` re-embeds drawers that already lack a vector — idempotent, safe to re-run, and a no-op on a healthy palace (it does not even resolve an embedder when there is nothing to repair)
