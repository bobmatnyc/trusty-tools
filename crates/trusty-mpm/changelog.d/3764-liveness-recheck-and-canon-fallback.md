Fixed

- `foreign_active_claim`'s nonexistent-path fallback now canonicalizes
  through the nearest existing ancestor and lexically normalizes the result,
  instead of comparing raw, un-normalized strings — closing a fail-open gap
  where a phantom `Active` record spelled with a `..` segment or reached via
  a symlinked ancestor slipped past the #3764 guard undetected (#3764).
- `dedup_stale_duplicates`'s `plan_dedup` loser branch now re-probes tmux
  liveness immediately before `decommission_dedup_loser`, matching the
  recheck the `#3396` (`workspace_dup_losers`) branch already ran — a loser
  whose tmux session went live between planning and the destructive call is
  now skipped instead of killed (#3764).
