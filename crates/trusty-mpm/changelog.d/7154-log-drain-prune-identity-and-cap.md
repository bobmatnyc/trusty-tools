Fixed

- (#7154) `PruneCandidate` is now keyed by the destination's stable `cache_namespace`/project identity instead of its position in `ResolvedLogDrain::destinations`, so a config reorder between ticks can no longer transfer one destination's armed prune state to a different destination that now happens to sit at the same index; a candidate whose identity no longer resolves in the current tick's plan is dropped rather than resolved against stale state. A batch the sanity cap refuses is now reported through `LogDrainDestinationStatus` without marking the destination `Failed`.
