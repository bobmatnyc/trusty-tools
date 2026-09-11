Fixed

- `project_register` now adopts the worktrees a project's checkout already has, so a repo that had agent activity before it was registered is surveyed and reclaimable instead of invisible to `tm session reconcile-worktrees`, `prune-worktrees --merged-prs`, `tm doctor` and the Disk survey (refs [#7357](https://github.com/bobmatnyc/trusty-tools/issues/7357))
