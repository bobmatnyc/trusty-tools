Documentation

- **BASE-AGENT states the post-merge cleanup rule agents were skipping.** `gh pr merge --delete-branch` only removes the remote branch, so the local worktree and branch were being left behind after every merge. The Git Workflow section now says: remove the worktree first, then use `gh pr view <branch> --json state` — never git's own ancestry check, which under-reports every squash merge and gets worse from a stale local checkout — as the sole merged-ness test before `git branch -D` ([#5768](https://github.com/bobmatnyc/trusty-tools/pull/5768))
