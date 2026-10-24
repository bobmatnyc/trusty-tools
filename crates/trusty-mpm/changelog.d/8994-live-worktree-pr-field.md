Breaking
- `core::worktree_ledger::fold::LiveWorktree` gains a public `pr: Option<u64>` field, the pull request a `pr_linked` event named for the tree's branch (#8994). Code that builds a `LiveWorktree` with a struct literal must set it (`None` while unknown). The JSONL ledger format is unchanged.
