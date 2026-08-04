Added

- Per-subagent context-cost guard at the `PreToolUse` hook ([#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837)) — a subagent's accumulated context is measured from its own transcript and classified against the new `[agent_cost]` section of `~/.trusty-mpm/config.toml` (`enabled`, `warn_tokens`, `max_tokens`)
  - ships WARN-ONLY: warns at 250k, and the hard stop is opt-in (`max_tokens = 0` by default). Sized against the measured population rather than one sample — across the 600 most recent subagent transcripts on a working machine over 14 days the distribution is p50 136k, p90 268k, p95 323k, with 18/600 (3.0%) at or above 400k, so a shipped-on 400k ceiling would deny roughly one dispatch in 33
  - the warning reaches the agent, once, as `hookSpecificOutput.additionalContext` — no `permissionDecision` is emitted, so the tool call still goes through the normal permission flow
  - when an operator opts into the stop, `SendMessage` and `git add`/`commit`/`push`/`status`/`diff` stay permitted so a stopped agent can always save and report its work; every segment of a composed Bash command must be one of those, so work cannot be smuggled behind an allowed verb
  - measured from a bounded tail read (64 KiB, retried once at 1 MiB when the smaller window holds no complete usage record), so the check costs the same on a 1 MB transcript as on a 500 MB one
  - fails open at every step: the PM is never evaluated, and an unreadable transcript, a missing usage record, or `max_tokens = 0` all allow the call
