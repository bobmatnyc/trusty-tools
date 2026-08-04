Added

- Per-subagent context ceiling enforced at the `PreToolUse` guard ([#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837)) — a subagent carrying more than `agent_cost.max_tokens` of context is denied its next tool call and told to report back for re-dispatch
  - configurable via the new `[agent_cost]` section of `~/.trusty-mpm/config.toml` (`enabled`, `warn_tokens`, `max_tokens`); defaults warn at 250k and stop at 400k, chosen to catch every overrun in #4837's evidence table while clearing normal agent lifetimes by ~3.5x
  - measured from the subagent's own transcript via a bounded tail read, so the check costs the same on a 1 MB transcript as on a 500 MB one
  - fails open at every step: the PM is never evaluated, and an unreadable transcript, a missing usage record, or `max_tokens = 0` all allow the call
