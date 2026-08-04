Security

- boot reconcile no longer indexes gitignored files when a git probe fails (closes [#4733](https://github.com/bobmatnyc/trusty-tools/issues/4733))
  - `head_sha()` returns `None` for a repo git merely declined to read — a stale worktree gitlink, `detected dubious ownership`, an unreadable `.git` — and reconcile dropped into the mtime catch-up walk meant for genuinely non-git roots. That walk honours `SKIP_DIRS` but not `.gitignore`, so previously-excluded files entered the corpus and became retrievable through the `search` and `grep` MCP tools
  - a new three-state `core::git::probe_work_tree` gates it: only a CORROBORATED `NoRepo` still takes the mtime path; a present-or-unknown work tree gets the gitignore-honouring full background reindex the sibling git-diff path already used
  - the exit code is not a classifier (git exits 128 for every fatal, and a bare repo exits 0 printing `false`), so the probe matches only the parenthesised `not a git repository (or any of the parent directories)` stderr and corroborates it against an ancestor `.git` witness
