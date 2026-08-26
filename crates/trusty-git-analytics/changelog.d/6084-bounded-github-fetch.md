Fixed

- **GitHub collection is bounded under secondary rate limits (#6084)** — both
  fetch paths could sustain a 429 storm indefinitely: PR-review collection
  retried each of 2625 pull requests on a fixed 1s/2s/4s ladder that never read
  `Retry-After`, and `fetch_on_reference` (default `true`) issued one Issues-API
  call per unique `#N` in commit history — 3681 of them on this repository —
  swallowing each rate-limit response as "this issue carries no classification
  signal". Nothing carried state between requests, so every remaining item paid
  four more rejected calls; a self-audit sat in that loop for 45+ minutes. Both
  paths now share a run-wide `FetchBudget`: `Retry-After` (and a drained
  `x-ratelimit-remaining`) drives the wait, each wait is clamped to 60s and
  charged against a 120s per-run allowance, and exhausting the allowance latches
  a breaker so every later call fails immediately without sending a request.
  Paginated walks stop at 100 pages and reference lookups at 500 per batch.
  Every cap and every early stop is reported — as a `CollectionFault` on the PR
  and reviewer passes, and as `IssueBatch::stopped_early` on the reference path —
  so a trimmed result is never presented as a complete one. A rate-limited
  reference is no longer cached as a resolved "no signal".
