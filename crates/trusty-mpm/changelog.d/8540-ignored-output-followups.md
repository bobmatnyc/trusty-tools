Fixed

- Agent-worktree reaping, `tm pr cleanup` and the shared worktree remover no
  longer treat a user file whose name only starts with
  `.claude/settings.local.json` (for example `settings.local.json-notes.md`)
  as harness output. Before, such a file was excused and then deleted with
  the tree. Only the exact settings file names, their `.bak`, timestamped
  snapshots and atomic-write staging names are excused.
- A `settings.json.tmp.<pid>.<seq>`, `settings.json.bak.<pid>.<seq>` or
  `settings.json.<pid>.<seq>.tmp` file left by a crashed write no longer keeps
  a worktree dirty forever.
- A FIFO or other non-regular file named `CLAUDE.md`, `.claude/settings.json`
  or `.gitignore` no longer hangs the provisioning-ledger snapshot a launch
  takes, or the decommission `?? .gitignore` checks. tm checks the file type
  without blocking and treats such a file as user content.
- A symlinked untracked `.gitignore` is no longer excused as provisioning
  output on the strength of its target's lines; the provisioning checks never
  follow a symlink.
