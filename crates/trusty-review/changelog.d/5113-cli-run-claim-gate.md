Fixed

- `trusty-review run` against a GitHub PR now opens the durable dedup claim store, so a re-run against the same `(owner, repo, pr, head_sha)` no longer posts a second comment. `build_deps_async` takes a `DedupNeed` and `dedup_need_for` derives it from the diff source and `allow_posting`; local diff sources still open nothing. `run_review` additionally aborts before any network call when posting is reachable and no store is present, so the combination fails closed wherever it is expressed. See #5113.
