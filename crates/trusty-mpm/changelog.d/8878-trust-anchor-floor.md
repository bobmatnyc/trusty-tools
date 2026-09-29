Added

- `tm hook --pm-guard` denies a write to a trust anchor —
  `~/.trusty-mpm/config.toml`, and `~/.trusty-mpm/twin/armed/*.json` while
  any exist — from every session except the Architect's main thread. It reads
  the edit tools, the shell write classifier, the destinations of `cp`, `mv`,
  `ln`, `install` and `sed -i`, and the sources of `ln`, `mv`, `cp -l` and
  `cp -s`, following symlinks and hard links and comparing anchor names
  without case. A directory above an anchor, `twin/` and `twin/armed/`
  included, is protected too. A `cp`/`mv`/`ln`/`install`/`sed -i` run by
  `xargs` is denied unless it is `cp -t DIR` or `install -t DIR` into a
  directory that leads to no anchor. The deny holds under
  `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`, and fails
  closed on an unknown home, an unresolvable target, and an unreadable
  payload (#8878).
