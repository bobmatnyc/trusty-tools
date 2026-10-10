Changed
- `secrets.resolve` also judges the resolving caller's own ancestry: a caller with a Claude Code ancestor reads only keys flagged "agents may use", even from a grant a non-agent registered for it (#9070). An unreadable ancestry refuses with `grant_refused`.
- `ProcessTable::is_agent` has a default body that answers `ProcessError::Unreadable`, so a process table that does not implement it fails closed in `has_agent_ancestor` instead of failing to compile (#9070).
