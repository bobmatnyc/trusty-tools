Changed

- `tm hook --pm-guard` now treats a delete of a trust anchor as an anchor
  write: `rm`, `unlink`, `trash`, `rmdir` (with `-p`, each parent),
  `shred`, `truncate`, and a `find` that deletes (`-delete`, or `-exec`,
  `-execdir`, `-ok` or `-okdir` running a delete verb, `mv` or a shell) are
  denied on `~/.trusty-mpm/architect-launch/` and its `.architect` and
  `.architect-session` records, `~/.trusty-mpm/config.toml`, the arming
  records under `~/.trusty-mpm/twin/armed/`, and any directory above them,
  for every session but the Architect's main thread. Wrapped forms
  (`command rm`, `env rm`, `sudo rm`) are read the same way; a delete run
  by `xargs`, or one whose path depends on a `cd`, a variable or a glob and
  could name an anchor, is denied because the guard cannot place it
  (#8878).
