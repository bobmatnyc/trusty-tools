Added

- **The `--api`/`--serve` daemon can now periodically upload its own log
  directory (#6537).** A new `[log_drain]` section in
  `~/.trusty-agents/config.toml` (same field names as trusty-mpm's
  `log_drain:` config section) drains `~/Library/Logs/trusty-agents/` via
  `trusty_common::log_drain::run_once`, scoped to the current project's
  `owner`/`project` (from its git origin, or an explicit override).
  Disabled by default; a malformed section is a hard error rather than a
  silent skip.
