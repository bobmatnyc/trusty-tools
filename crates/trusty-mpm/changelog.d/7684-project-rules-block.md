Added
- Every `Agent` dispatch now carries a `## Project rules (from CLAUDE.md)` block: the regions of the project CLAUDE.md marked `<!-- tm:agent-rules -->`, or the whole file when it marks none. Subagents started with `omitClaudeMd: true` keep the project build and test rules (#7684).
- The project rules block now reaches every tm-managed dispatch: a pm-guard bypass (`TRUSTY_MPM_DISABLE_HOOKS`, `TRUSTY_MPM_PM_UNRESTRICTED`, divert workers) still appends it, and with `[pm_guard] enabled = false` the always-installed `tm hook --egress-guard` appends it instead. `Task` dispatches get it too (#7684).
- An unreadable `CLAUDE.md` puts a one-line `project rules unavailable: <reason>` notice in the block instead of dropping the rules silently (#7684).
