# Harness Understanding — t-code-as-Overseer Contract (Forward-Looking)

> DOC-21 `SPEC-HARNESS-UNDERSTANDING-01~draft` | Section: Overseer Contract
>
> **Status:** Forward-looking. The `Overseer` trait and `HarnessSource::Code`
> slot are defined; this section governs the future wiring when tcode becomes
> an active overseer. No emitters exist yet.

## The Overseer Seam

The overseer seam consists of two components already in place:

1. **`Overseer` trait** (`crates/trusty-mpm/src/core/overseer.rs`): the
   strategy interface the daemon dispatches hook events to. Returns
   `OverseerDecision::{Allow, Block, Respond, FlagForHuman}`.

2. **`HarnessSource::Code`** (`crates/trusty-agents-common/src/events/lifecycle.rs`):
   the reserved source tag for tcode-originated `HarnessEvent`s. No emitters
   exist yet; this slot is reserved for the future t-code overseer.

## Event-to-Decision Mapping

A t-code overseer subscribes to the `HarnessEvent` bus filtered on
`HarnessSource::Code` and maps events to `OverseerDecision` using the agnostic
WHEN-TO-INTERVENE protocol:

| Observed event | Recommended decision | Rationale |
|----------------|---------------------|-----------|
| `pm_thinking` — routine reasoning | `Allow` | Normal Working state; no intervention needed |
| `agent_message` — within task scope | `Allow` | Expected output |
| Permission dialog detected in pane | `Respond` (if authorized) or `FlagForHuman` | Decision gate |
| Error event + non-recoverable pattern | `FlagForHuman` | Human must decide next step |
| Sensitive file write outside scope | `Block { reason }` | Scope violation |
| Unexpected tool call (out-of-scope) | `Block { reason }` | Scope enforcement |
| Long silence (>5 min) in Working state | `FlagForHuman` | Possible hang |

## `__HARNESS_EVENT__` as Structured Input

For a t-code overseer, `__HARNESS_EVENT__` lines are the primary structured
input (vs. pane text for Claude Code). The overseer SHOULD prefer event-stream
classification over pane-text heuristics when events are available.

Parse events as `HarnessEvent` (strip `__HARNESS_EVENT__ ` prefix, parse JSON).
Filter on `source == HarnessSource::Code` and the session ID being overseen.

## `FlagForHuman` Escalation Policy

The overseer MUST escalate (`FlagForHuman`) in at least these cases:
- Permission dialog that the overseer is not authorized to answer
- Any action that would mutate protected files (outside declared scope)
- Session silent for >5 minutes with no completion evidence
- Unexpected process exit (non-zero exit code without a prior error event)

The `FlagForHuman.summary` field must be a concise, actionable description:
`"Session s-abc has been silent for 6 minutes; last event was pm_thinking"`

## Future Wiring

When a t-code overseer is implemented:
1. Subscribe to the `HarnessEvent` bus at startup (filtered on `HarnessSource::Code`)
2. Register as the `Overseer` in the daemon's `DaemonState`
3. For each hook event: apply this contract to produce an `OverseerDecision`
4. Emit `HarnessPayload::Hook { kind, data }` events for audit purposes

The `HarnessSource::Code` source tag transitions from "reserved" to "active"
when the first real emitter lands. This spec governs the behavioral contract
from that moment forward.
