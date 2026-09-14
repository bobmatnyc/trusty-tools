Fixed
- Worktree reclaim gate 2 no longer lets a live session's PROJECT-ROOT workspace claim veto a worktree nested under it; only a claim on the worktree itself, or inside it, still refuses (#7652).
- The merged-PR reclaim sweep refreshes each repository's remote-tracking refs once before judging unsaved work, so a branch whose pull request squash-merged on GitHub — including one continued on an `-r2` branch — is no longer miscounted as holding unpushed commits (#7889).
- Every worktree removal route writes one audit line naming the path, branch, owning session or agent, and the reason, emitted before the deletion is attempted rather than on its success arm (#7885).
- The merged-PR reclaim pass bounds its byte-measurement phase instead of running it unbounded, which is where `tm session prune-worktrees --merged-prs` hung for minutes at near-zero CPU under host load; classification stays unbounded (#7884).
