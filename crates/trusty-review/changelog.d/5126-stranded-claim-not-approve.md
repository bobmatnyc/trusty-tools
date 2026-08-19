Fixed

- A dedup claim another process still holds no longer reports `APPROVE`. `DedupStore::claim` now returns `ClaimOutcome::InProgressElsewhere` for a fresh in-progress record and keeps `Skipped` for a completed one, so the runner tells a review that never ran apart from one that ran and finished; the blocked run reports UNKNOWN with the reason instead of a verdict. Previously a process that died between `claim` and `complete` made every re-run of that PR report approval for up to two hours. See #5126.
