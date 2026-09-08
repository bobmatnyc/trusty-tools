Added

- **`tcode serve` now writes a durable file log and drains it periodically
  (#6537).** `init_tracing_with_file_log` adds a daily-rotating file layer
  under `~/.trusty-code/logs/tcode.log.*` alongside the existing stderr
  layer. When `~/.trusty-code/log_drain.yaml` sets `enabled: true` and a
  `destination` (same field names as trusty-mpm's `log_drain:` config
  section), the daemon periodically uploads that log via
  `trusty_common::log_drain::run_once`, scoped to the project `tcode serve`
  is bound to (or an explicit `owner`/`project` override). Disabled by
  default; a malformed section is a hard error rather than a silent skip.
