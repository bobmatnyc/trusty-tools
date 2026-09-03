Fixed

- `tm session prune-worktrees --merged-prs` reclaims a worktree whose branch squash-merged from two or more commits. The previous fix compared patch ids one commit at a time, and a squash carries the union of the branch's patches, so it matched none of them and the unsaved-work gate reported the landed work as unsaved (#6507).
- The merged-PR pass names why every non-reclaimable candidate was refused, each candidate exactly once: a tree a dispatched agent holds keeps its agent-skip line, and every other refusal reads `<path>: blocked at <gate>: <reason>`. A failed per-branch `gh pr list` logs at WARN with the branch, the repository, and the resolved `gh` identity. Only the agent-ownership gate's refusals were visible before (#6507).
