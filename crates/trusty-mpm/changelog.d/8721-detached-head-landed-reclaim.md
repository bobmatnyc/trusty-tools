Fixed

- A clean worktree with a detached HEAD whose content is already on
  `origin/main` can now be removed. `version-control`'s guarded
  `git worktree remove` and `tm session prune-worktrees --merged-prs` both
  run the landed-content check on such a tree once no merged pull request
  names its commit. Before, both refused it for having no branch. A detached
  tree holding content `origin/main` lacks is still refused, and the refusal
  names the first such path. A refresh of `origin` that fails still refuses,
  and so does a pull request search that does not answer: the sweep now
  reports that search as a failed lookup.
