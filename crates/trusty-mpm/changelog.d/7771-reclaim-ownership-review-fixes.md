Fixed
- A Claude session recorded by more than one managed session is judged by every record: one live record keeps its tree, and the tree is reclaimable only when every record has ended. (#7771)
- `tm pr cleanup` and the supervisor's cleanup sweep find a session's claim when its workspace path names the tree through a symlink or macOS's `/private` prefix. (#8301)
- `tm pr cleanup` ends a tree's claims only after every pre-removal check passes. A tree kept by the ownership re-check, the unsaved-work re-check or the lock check keeps its claims. (#8301)
- `tm pr cleanup` judges a tree's harness lock again at the moment it would release it. A lock re-taken by a running agent, or one that cannot be judged, keeps the tree. (#7771)
