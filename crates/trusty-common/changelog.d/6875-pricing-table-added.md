Added

- New unconditional `pricing` module and bundled `pricing.toml`: the one model
  pricing table for the workspace, replacing the independent tables in
  `trusty-agents` and `trusty-mpm`. Rows carry `input` / `output` /
  `cache_write` / `cache_read` in USD per million tokens plus an
  `effective_from` date, so a ledger can price last month's usage at last
  month's rate. `Pricing::rate_for` normalises the routing spellings the
  providers actually pass (bare ids, `us.anthropic.*` Bedrock ids, OpenRouter's
  `anthropic/claude-*`, dated and `-v1:0` snapshots) and returns `None` for an
  unpriced model rather than a fallback rate; `warn_unknown_model_once` reports
  the gap. `Pricing::with_override` layers an operator file at
  `~/.trusty-tools/pricing.toml`. The table is embedded with `include_str!`, so
  the crate has no runtime file dependency. (#6875)
