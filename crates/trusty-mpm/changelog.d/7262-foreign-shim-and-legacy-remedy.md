Fixed

- `hooks_foreign_conflict` reported CLEAN while claude-mpm's `claude-hook` shim
  raced tm's `PreToolUse` rewrite in a real project. The check asked whether the
  command string spelled `claude-mpm` anywhere, and the shim is registered by
  bare name off `PATH`, so nothing in it named the owning harness. It now
  resolves what the command actually invokes and classifies by the executable's
  file name as well. `legacy_overrides` has no `--fix` arm — repairing it means
  deleting a tracked file, which needs an owner ruling — so its failure text now
  prints the exact `git rm` command over the files it found instead of leaving
  the operator to assemble one (#7262).
