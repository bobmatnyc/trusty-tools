Changed

- The `disk_bytes` health metric is recomputed every 60 s instead of every 10 s,
  matching `trusty-search`. Walking the data root six times less often cuts
  exposure to the concurrent-mutation race behind #4764 by the same factor, at
  no user-visible cost for an at-a-glance footprint figure (#4764)
