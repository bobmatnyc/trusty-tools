Changed

- `BASE-AGENT.md` gains a `Never Narrate a Wait` section, placed directly before `Git Workflow` so an agent reads it before it starts work ([#5019](https://github.com/bobmatnyc/trusty-tools/pull/5019))
  - a subagent's turn ends the moment it stops emitting tool calls, and nothing re-invokes it afterward — `CLAUDE_SUBAGENT_BG_SHELL_MAX_MS` lets a subagent-owned background shell outlive the turn, but no completion event reconnects the two. Ending a turn with "I'll wait for the pull", "will resume when the monitor reports completion", or "monitoring in the background" strands the task until a human notices; observed five times in one session
  - to await a long operation the agent stays in the turn: start it with `run_in_background`, then poll an until-loop against its output file. The loop is backgrounded too, because foreground `sleep` is blocked in this harness — a prior revision's bare "poll" advice was itself producing the parking it meant to prevent
  - reporting unfinished state and stopping ("still pending: head SHA abc1234, 10 checks unsettled") is now stated as a correct, complete outcome; the failure is stopping while implying you will continue
  - never re-issue a long-running command because a foreground shell call returned early at the ~120s cap and auto-backgrounded — check whether the original is still running first
  - the now-redundant "never spawn a background monitor as a wake mechanism" bullet under `Your own gates DO block, in the foreground` collapses to a pointer at the new section
