Added
- `host_metrics::history` — a bounded FIFO of metric samples behind the console's
  real-time graphs. `MetricRing::push` is the single write path, so a sample the
  console takes itself and one a service pushes (#6284) enter the buffer
  identically; at capacity it evicts the oldest and preserves insertion order.
  `HOST_HISTORY_CAPACITY` (120) and `HOST_SAMPLE_INTERVAL_SECS` (5) pin the
  owner's 10-minute window (#6641).
