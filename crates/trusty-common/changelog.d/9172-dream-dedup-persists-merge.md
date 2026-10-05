Fixed
- Dream dedup writes the merged survivor to the palace store before it deletes the loser, so merged text survives a reopen; if that write fails, both drawers stay. The merge no longer cuts text at 500 bytes, and appends nothing when the survivor already holds the loser's text (#9172).
- Dream dedup picks the current drawer as survivor: a `fact_key` slot holder, then a `ruling`-tagged drawer, then the newer `created_at`, then the higher importance. Two slot holders are never merged (#9172).
- Forgetting a drawer that the maintenance journal names as a dedup survivor writes a `forget_of_merged_survivor` record (#9172).
