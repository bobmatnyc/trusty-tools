Fixed

- Every git subprocess trusty-mpm spawns on the in-project clone/fetch/
  worktree-add hot path, the catalog-sync provisioner, and the crate's
  hardened `git_command` entry point now routes through
  `trusty_common::git`, which disables git's own auto-maintenance/gc for
  that invocation. Prevents a fleet of session worktrees from each
  independently triggering a `git maintenance run --auto` repack against
  the shared base clone's object store (#7171).
- New `core::git_maintenance::disable_and_log`: idempotently writes
  `maintenance.auto=false` / `gc.auto=0` into a managed checkout's local
  git config at clone time (`ensure_base_clone`) and at explicit project
  registration (`mpm.projects.register`), so operator-run git inside a
  worktree cannot re-trigger the storm either (#7171).
- New `tm doctor` probes `maintenance_config` and `maintenance_processes`:
  warn when a registered base clone carries more than 3 worktrees without
  `maintenance.auto` pinned off, and when more than one `git maintenance
  run` process is live on the host at once (#7171).
