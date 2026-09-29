Added

- `tm hook --pm-guard` denies a write to a trust anchor —
  `~/.trusty-mpm/config.toml`, and `~/.trusty-mpm/twin/armed/*.json` while
  any exist — from every session except the Architect's main thread. It reads
  the edit tools, the shell write classifier, and the destinations of `cp`,
  `mv`, `ln`, `install` and `sed -i`, following symlinks and hard links. The
  deny holds under `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`,
  and fails closed on an unknown home, an unresolvable target, and an
  unreadable payload (#8878).
