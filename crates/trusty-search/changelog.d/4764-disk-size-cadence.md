Changed

- The `disk_bytes` health metric is recomputed every 60 s instead of every 10 s.
  Walking a multi-GB, actively-mutating data directory six times less often
  cuts exposure to the reindex/prune race behind #4764 by the same factor, at
  no user-visible cost for an at-a-glance footprint figure (#4764)
