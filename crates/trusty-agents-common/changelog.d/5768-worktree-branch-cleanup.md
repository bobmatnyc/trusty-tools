Documentation

- **BASE-AGENT states the post-merge cleanup rule agents were skipping.** `gh pr merge --delete-branch` only removes the remote branch, so the local worktree and branch were being left behind after every merge. The Git Workflow section now says: remove the worktree first, then delete the local branch with `git branch -d`, falling back to `-D` only once the merge is confirmed via `gh pr view` — never blind-forced ([#5768](https://github.com/bobmatnyc/trusty-tools/pull/5768))
