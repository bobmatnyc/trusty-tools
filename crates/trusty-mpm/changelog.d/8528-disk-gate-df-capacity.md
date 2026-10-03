Changed

- The worktree disk-usage gate now compares `disk.max_usage_pct` against the
  Capacity figure `df` prints for the mount (`used / (used + available)`, via
  `statvfs`), instead of `sysinfo`'s "available for important usage". An
  existing threshold therefore fires at a different point: one mount read 93%
  before and reads 74% now, so a value tuned against the old figure refuses
  later than it did. A refusal now states the used and available bytes it
  measured.
