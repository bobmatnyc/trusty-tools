Changed
- PM guidance: every `SendMessage` resume of a worktree agent, not only a CI hand-back, first checks `git worktree list`; a gone tree gets a fresh `isolation: "worktree"` dispatch that restates the base commit and branch (#8004).
