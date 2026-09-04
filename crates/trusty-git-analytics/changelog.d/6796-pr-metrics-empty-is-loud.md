Fixed

- `tga pr-metrics` no longer returns success on a header-only CSV. The artifact is
  still written, then the command fails with a message naming which case produced
  it: an empty `pull_requests` table (pointing at `github.fetch_prs`, #211, and a
  missing non-interactive git credential, #6244), a lookback window that excluded
  every stored PR, or rows that were read and aggregated to nothing. Every
  repository of a 56-repo bundle shipped an empty `pr-metrics.csv` and the run
  reported success; `audit::run_full_sweep` records this as a stage failure, so it
  reaches the report's Gaps & Caveats without aborting the sweep (#6796).
- A pull request whose author login is empty — GitHub's answer for a deleted
  account — is counted under an `(unknown)` bucket instead of being skipped, so it
  no longer vanishes from the opened, merged, and cycle-time totals (#6796).
