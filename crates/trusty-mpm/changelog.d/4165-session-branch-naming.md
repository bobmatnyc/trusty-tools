Fixed

- **A session provisioned from a repo URL no longer leaks its git branch.** `provisioner::workspace::provision_in` named the worktree's branch bare (`<session-id>`) while `session_manager::decommission::remove_session_worktree` force-deletes `session/<session-id>`, so the delete missed and one branch accumulated in the base clone per session. The provisioner now names the branch through `core::worktree_naming::worktree_branch_for`, the single convention both sides read ([#4165](https://github.com/bobmatnyc/trusty-tools/issues/4165))
