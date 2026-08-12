Fixed

`tm session start` and `tm connect` no longer seed the operator's real
`~/.claude.json` and `~/.claude/settings.json` when given an isolated
`FrameworkPaths` base; both now resolve from that base, as `FrameworkPaths`
already promised. Alongside it, the `tm` binary's own test suite stopped
repointing `$HOME` and `$CLAUDE_CONFIG_DIR` process-wide — a write that could
straddle a parallel test's agent-roster scan and report a truncated roster — and
a mechanical guard now fails the build if either comes back (#5544).
