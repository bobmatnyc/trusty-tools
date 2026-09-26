Fixed

- `tm session prune-worktrees --merged-prs` no longer reclaims a worktree
  whose branch has no merged pull request of its own unless its content is
  already on the base (the `merge-tree` no-op check). A head-commit match to
  another branch's pull request, commits sitting on some remote ref, or a
  failed or timed-out `gh` lookup now keeps the tree, and the refusal names
  why. A no-PR tree reclaimed on landed content logs that as its reason.
- A worktree the merged-PR sweep reclaims now has its local branch deleted
  too, only when its content is proven landed, its tip is still the exact
  commit that proof judged, and no other worktree (the main checkout
  included) has it checked out. The deletion is one compare-and-delete
  (`git update-ref -d` at the proven commit), so a commit made after the
  proof keeps the branch. A failed branch lookup keeps the branch rather
  than reporting it gone. Each deletion or kept branch is logged. The landed-content proof
  (up to 40 s) now runs once per candidate before deletion, not twice.
