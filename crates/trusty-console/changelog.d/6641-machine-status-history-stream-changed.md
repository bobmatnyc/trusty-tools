Changed
- History is in-memory only and resets on restart, by owner ruling — a restarted
  console begins a new, empty window (#6641).
- `host_status::start` is gone; one loop in `machine_history::sampler` now writes
  both the point-in-time host cache and the history ring, so the two can never
  disagree about the newest sample. `host_status::HostMetricsCache` is unchanged
  (#6641).
