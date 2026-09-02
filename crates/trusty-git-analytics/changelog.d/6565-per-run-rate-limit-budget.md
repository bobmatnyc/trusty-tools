Fixed

- `tga collect`'s rate-limit sleep allowance is now per-RUN rather than
  per-client (#6565). #6084 gave each GitHub client a `FetchBudget`, which bounds
  a client and not a run: one `collect` builds several — org discovery, the PR
  sweep, the reviewer pass — so the 120 s ceiling was charged once per client and
  the wall-clock a run could spend asleep scaled with the number of clients
  instead of being the fixed bound the constant reads as. A breaker latched
  during one pass also did not stop the next from spiralling again. The new
  `RunBudget` is a shared handle every client takes via
  `GitHubClient::with_run_budget`, constructed once on `CollectionPipeline`, so
  there is one allowance, one breaker, and one truncation ledger for the whole
  run.
- `TGA_RATE_LIMIT_SLEEP_BUDGET_SECS` overrides the 120 s total. The same ceiling
  now covers strictly more work than it did per-client, so a long multi-org sweep
  that legitimately needs a larger allowance has a way to ask for one. A zero or
  unparseable value falls back to the shipped default rather than latching the
  breaker on the first rate-limited response.
