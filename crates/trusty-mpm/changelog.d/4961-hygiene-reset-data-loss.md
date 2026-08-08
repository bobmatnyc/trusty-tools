Fixed

- startup hygiene no longer destroys gitignored working-tree content in the managed checkout (closes [#4961](https://github.com/bobmatnyc/trusty-tools/issues/4961))
  - the update step is now a non-destructive `git merge --ff-only`, never `git reset --hard`, and it refuses when any path the update would write already exists untracked on disk — `git status --porcelain` never reported gitignored paths, so a gitignored file holding real content read as clean and was silently overwritten
  - the update also refuses off the default branch, where the ahead-count validated `origin/<checked-out>` while the operation moved the branch to `origin/<default>`
  - a `.trusty-mpm-no-hygiene` marker file in a base clone now opts that single checkout out of the sweep; previously the only control was the process-wide `TRUSTY_MPM_INPROJECT_HYGIENE` env var
