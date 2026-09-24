Fixed
- `tm pr merge` and `tm pr cleanup` no longer end another live session's claim or remove its agent worktree. A tree is removed only when every claim holder is the calling session or a provably ended one, and the #7771 ownership rule passes; a kept tree names its reason. (#8301)
- `tm pr cleanup` re-reads a tree's claims and ownership immediately before it unlocks and removes the tree. A session that claimed the tree, or an agent that re-locked it, while cleanup was running keeps it; claims that cannot be re-read keep it too. (#8301)
- A merge-scoped cleanup no longer reports "no worktree holds <branch>" when trees on the head commit were left in place. (#8301)
