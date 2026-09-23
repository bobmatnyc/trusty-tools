Fixed
- `tm session decommission` (and `tm sessions decommission`) now exits non-zero when it keeps a workspace it could have removed, and names what blocked the removal; the daemon's decommission response carries that reason as `workspace_kept_reason` (#7660).
- `tm session decommission --force` removes an in-project worktree that is dirty only from tm's own provisioning files (`.gitignore`, `.claude/settings.json`, `.claude/settings.json.bak`, `CLAUDE.md`). It still refuses any other modified or untracked file, unpushed commits, and a dirty check that cannot complete (#7660).
