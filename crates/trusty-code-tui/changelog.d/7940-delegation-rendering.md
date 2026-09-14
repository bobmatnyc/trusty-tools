Added

- `ReplEvent::DelegationStarted`/`DelegationFinished` (with `DelegationOutcome`) and `ReplEvent::AgentOutput` — the engine-agnostic seam for rendering a delegated sub-agent's block, and for streaming output keyed by `(agent_id, turn_id)` so a primary agent and a sub-agent streaming at once no longer interleave into one chat bubble (#7940).
- `ChatRole::Delegation`/`ChatRole::Delegated` and their scrollback rendering: a `▶ <agent> — <task>` header, gutter-prefixed sub-agent output and tool notices, and a `└ <agent> — <outcome>` footer. The status line names the working sub-agent while a delegation is open and reverts when it closes, with no engine push either way.
- `ReplEvent::ToolInvocation` gained `agent_id`, so a delegated agent's tool calls render inside its block instead of at top level.
