Added

- `crate_config::save_raw_at` writes caller-supplied bytes to a config path
  atomically — temp sibling, then rename — for a caller that already holds the
  exact text to land and cannot go through `save_at`'s serialise-and-header
  path. `tm issue seed-config` appends its `agents.ticketing` block textually to
  preserve an operator's comments, and now routes that write here instead of a
  truncating `std::fs::write`, so an interrupted seed leaves the existing config
  byte-identical rather than half-written. `save_at` delegates to it, keeping one
  atomic-config-write implementation in the workspace (#7067).
