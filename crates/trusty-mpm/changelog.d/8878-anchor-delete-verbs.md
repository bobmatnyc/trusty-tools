Changed

- `tm hook --pm-guard` now treats a delete of a trust anchor as an anchor
  write: `rm`, `unlink`, `trash`, `rmdir` (with `-p`, each parent),
  `shred`, `truncate`, and a `find` that deletes (`-delete`, or `-exec`,
  `-execdir`, `-ok` or `-okdir` running a delete verb, `mv` or a shell) are
  denied on `~/.trusty-mpm/architect-launch/` and its `.architect` and
  `.architect-session` records, `~/.trusty-mpm/config.toml`, the arming
  records under `~/.trusty-mpm/twin/armed/`, and any directory above them,
  for every session but the Architect's main thread. Wrapped forms
  (`command rm`, `env -u X rm`, `sudo -u root rm`), commands after a
  reserved word (`if … then rm …`, `do rm …`, `{ rm …; }`, `! rm …`), brace
  groups (`rm -rf ~/{.trusty-mpm,x}`) and the command a `find -exec` action
  runs are read the same way, for the copy verbs too (#8878).
- The PM is also denied a delete the guard cannot place: one run by `xargs`
  (use `find DIR … -delete` or literal paths instead), one whose path
  depends on a variable, glob or `cd` and could name an anchor, a `find`
  from above an anchor, and an `rmdir -p` reaching `~/.trusty-mpm`. An agent
  or subagent is denied only a delete whose path resolves to an anchor, a
  directory holding one, or, for a glob, a matching entry that is one; an
  unknown thread is judged as the PM (#8878).
