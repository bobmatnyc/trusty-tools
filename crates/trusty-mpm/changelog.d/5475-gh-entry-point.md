Changed

- `core::gh_account`, `core::gh_account_enforce`,
  `core::session_launch::workstream_label`, and
  `session_manager::worktree_reclaim` invoke `gh` through
  `trusty_common::gh::GhCommand`
  ([#5475](https://github.com/bobmatnyc/trusty-tools/issues/5475)). Timeout
  bounds, `GH_CONFIG_DIR` scoping, the `GH_TOKEN` env stripping, and the
  `current_dir`-never-`-C` rule (#2919) are all unchanged — `worktree_reclaim`
  keeps its own kill-on-expiry runner and takes the unspawned command from the
  entry point.
