Changed

- Retired the blocking-CI-wait doctrine across every bundled instruction copy: agents now push, take a ONE-SHOT `gh pr view` / `gh pr checks` status read, report, and end their turn; the PM owns re-engagement when CI settles ([#4792](https://github.com/bobmatnyc/trusty-tools/issues/4792))
  - `gh pr checks --watch` is now forbidden — it streams check output into the agent's context (546k tokens over 54 minutes on one PR). The retirement is about context cost, not runnability
  - `BASE-AGENT.md` "Foreground Execution — NEVER End Your Turn To Wait" is replaced by "Finishing Work — Push, Report, Stop"; own-gate commands still block in the foreground
  - `version-control.md`, `local-ops.md`, `BASE-ENGINEER.md`, `tm-delegation-patterns.md`, and the `trusty-code` asset mirror updated to match
  - PM instructions section renamed "Parked-Subagent Re-Engagement": a hand-back with CI pending is correct behavior, not a park, and must not be nudged back into a blocking wait
  - `idle_nudge::DEFAULT_NUDGE_MESSAGE` no longer tells a stalled pane to run `gh pr checks --watch`
  - The one-shot read now documents two traps: `bucket` can report a false DONE under GitHub API eventual-consistency lag (cross-check `state`), and repeated `gh pr update-branch` is a treadmill that mints a new untested head each time (BEHIND is not a correctness gate)
