Added

- `tm doctor` reports every `CLAUDE.md`, `CLAUDE.local.md` and
  `.claude/CLAUDE.md` ABOVE the project root in a new `ancestor_claude_md` row,
  with each file's size and a bytes/4 token estimate. A tm seed template with no
  project content is `Fail`; a file with real content is `Warn`; a file already
  in `claudeMdExcludes` (read across the project, user and managed layers) is
  not a finding. `tm doctor --fix --yes` renames a pure seed template aside to
  `<name>.stale-seed-<YYYYMMDD>` and adds a content-carrying file to
  `claudeMdExcludes` in the project root's `.claude/settings.local.json`. The
  same scan emits one WARN at session launch and a stderr notice from
  `tm session instructions`. The project root is the NEAREST enclosing
  directory holding a `.git` entry or a `.trusty-mpm` directory, and `$HOME` is
  never a boundary, so a registered project under a dotfiles `$HOME` repository
  still reports `$HOME/CLAUDE.md`. A start directory that does not exist or
  cannot be read is an error on every surface — a `Warn` on the doctor row, a
  `Failed` repair step, a printed notice — never an empty scan. (#7673, folded
  from #7700)
