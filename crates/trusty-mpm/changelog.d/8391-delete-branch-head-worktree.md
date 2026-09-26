Documentation

- `tm-workflow.md` no longer recommends `gh pr merge --delete-branch` — a
  worktree holding the head branch (#8391) makes the flag fail post-merge the
  same way a worktree holding the base branch already did (#7104). The skill
  now names `tm pr merge` + `tm pr cleanup` as the sequence and adds the
  merge-tree check before a manual `git branch -D`
  (closes [#8391](https://github.com/bobmatnyc/trusty-tools/issues/8391))
- `cleanup_deferred_report`'s message now names the remote branch as possibly
  stranded too, not only the local one, matching the head-held failure mode
