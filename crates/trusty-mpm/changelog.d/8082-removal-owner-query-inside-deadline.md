Fixed
- The ADR-0057 `git worktree remove` re-check bounds its daemon owner query by the removal deadline (#8082). A late-starting guard facing a daemon that accepts and never answers now denies, naming `sole-owner`, inside the budget instead of after its own 2.5 s client timeouts. A regression test also pins the removal deadlines inside the guard hook's registered timeout.
