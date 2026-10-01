Added

- A `[pm_guard] enabled` key in `~/.trusty-mpm/config.toml` turns the PM guard
  off (owner ruling 307). With `enabled = false`, the launch, resume and
  `tm install` writers no longer register the `tm hook --pm-guard`
  `PreToolUse` entry, and they remove one a prior launch wrote. Every other
  managed hook, including the observability `tm hook`, is unchanged. A missing
  key, and a config file that cannot be read or parsed, keep the guard on. A
  running Claude Code session keeps the hooks it loaded at startup, so the
  change applies at its next launch (#9018).
- `tm doctor` has a `pm_guard` row. It reports `Ok` while the guard is on and
  warns "pm-guard disabled by [pm_guard] enabled = false" when the key turns
  it off (#9018).
