Changed
- The `version-control` agent now names `tm pr merge <n> --auto` as the merge
  path, so the PR body it wrote becomes the squash commit message instead of a
  concatenation of the branch's raw commit messages.
  `gh pr merge --squash --delete-branch --auto` remains the fallback on a host
  without `tm` ([#6808](https://github.com/bobmatnyc/trusty-tools/issues/6808)).
