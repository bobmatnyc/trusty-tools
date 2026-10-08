Added
- A machine-wide, append-only worktree ledger at `~/.trusty-mpm/worktrees.jsonl`. The daemon's in-project spawn and `tm launch --worktree` (and the daemon-unreachable guided fallback) record every worktree they create; a ledger they cannot write refuses the creation instead of producing an unrecorded tree (#8994).
- `tm worktrees [--json] [--no-size]` prints the count and GiB per project from the ledger. It first registers pre-existing worktrees of every registered project from `git worktree list` (Claude Code agent-isolation trees are recorded as observed), then measures each live tree unless `--no-size` is passed (#8994).
- `tm doctor` row `worktree_registry`: count and GiB per project, read from the ledger alone (#8994).
