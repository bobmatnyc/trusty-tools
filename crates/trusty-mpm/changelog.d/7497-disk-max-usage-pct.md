Added

- `disk.max_usage_pct` (integer percent, default 90) refuses a NEW worktree
  when the mount holding it is at or above that usage. Both creation surfaces
  are gated: daemon/CLI provisioning (`tm session new`, `tm launch`, the MCP
  `session_new`, the console — all of which funnel through
  `create_session_worktree`) and an agent's own `git worktree add` through the
  `tm hook --pm-guard` PreToolUse guard, which is checked ahead of the subagent
  exemption so an `Agent(isolation: "worktree")` dispatch is covered too
  (#7497).
- The refusal names the mount, the measured percent, the threshold and the
  config key, so it says what to change as well as what was refused. The
  measurement is the mount the path actually sits on — not the cross-mount
  aggregate, which stays healthy while one volume fills (#7497).
- Provisioning FAILS CLOSED on a mount it cannot measure (ADR-0037: an explicit
  worktree request that cannot be honoured is a failure), while the Bash guard
  fails OPEN with a warning, matching every other classifier in that guard
  family (#7497).
- A `disk.max_usage_pct` outside `1..=100` is rejected and reported rather than
  applied, falling back to 90 — a typo can neither refuse every worktree nor
  silently disable the gate. The rejection travels into the refusal and into
  `tm doctor`, which Warns naming the discarded value, so a `150` in the file is
  never reported as a configured `90`. An absent `disk:` section changes nothing
  (#7497).
- The gate has no ambient off switch: no environment variable disables it, and
  an unreadable config leaves the default 90% in force rather than lifting the
  gate. A test that needs a different answer writes an explicit
  `disk.max_usage_pct` in its own config home, or calls the ungated
  `create_session_worktree_unchecked` (#7497).
- `tm doctor` gains a `disk_usage` check: the worktree store's mount against
  the threshold — `Ok` below it, `Warn` at or above it (the gate is refusing),
  `Unknown` when the mount could not be measured (#7497).
