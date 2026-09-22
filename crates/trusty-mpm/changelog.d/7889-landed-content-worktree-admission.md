Fixed

- A clean worktree whose content is already on `origin/main` can be reclaimed even when GitHub has no MERGED pull request for its branch ([#7889](https://github.com/bobmatnyc/trusty-tools/issues/7889)) — the donor-branch shape, where the work landed through a sibling `-r2` branch's squash and no pull request will ever carry the parked branch's own name
  - both the ADR-0057 `git worktree remove` guard and `tm session prune-worktrees --merged-prs` run one shared `landed-content` predicate, so they cannot give one worktree opposite answers
  - fail-closed throughout: a failed or expired `origin` refresh, an unresolvable landing base, a `git merge-tree` error or conflict, a residual path, an open pull request, a dirty tree and a live owner all still refuse, and ancestry is never used as evidence
  - a refusal names the admission and the first path the merge would still change
  - on the sweep, a donor branch's commits that reach no `origin` ref no longer refuse on their own, because the content comparison judges them; an uncommitted file or a dirty nested repository still refuses, and the pre-delete re-check asks the admission again instead of demanding a merged pull request
