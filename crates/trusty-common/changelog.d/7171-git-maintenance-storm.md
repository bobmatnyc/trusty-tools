Added

- New `trusty_common::git` module: the workspace's single entry point for
  constructing a `git` subprocess. `git::command()` / `git::command_in()`
  (sync) and `git::tokio_command()` / `git::tokio_command_in()` (async) all
  carry `-c maintenance.auto=false -c gc.auto=0` on argv, preventing a fleet
  of worktrees from independently triggering `git maintenance run --auto`
  repacks against a shared object store (#7171).
