Added

- **`AppState::write_pipeline_budget` and `with_write_pipeline_budget`** — the daemon-wide ceiling on one write's critical section, mirroring the existing `write_op_budget` pair that bounds only the waits before the mutex is acquired ([#6366](https://github.com/bobmatnyc/trusty-tools/issues/6366))
