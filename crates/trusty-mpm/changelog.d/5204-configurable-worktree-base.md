Added

- The session-worktree base directory is configurable via `worktrees_dirname` in `~/.trusty-tools/trusty-mpm/config.yaml` or `TRUSTY_MPM_WORKTREES_DIRNAME`, defaulting to `.worktrees` (#5204). Creation uses the configured name; detection also keeps matching `.worktrees`, so retargeting never orphans worktrees already on disk.
