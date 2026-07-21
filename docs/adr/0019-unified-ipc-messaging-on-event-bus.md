# 0019. Unified IPC messaging on the event-driven control bus

- **Status:** Accepted
- **Date:** 2026-07-21
- **Scope:** Workspace-wide (trusty-mpm, trusty-code, trusty-agents)
- **Reversibility Cost:** High — both deprecated channels have callers; migration requires per-harness adapter and coordination across PMs
- **Decision Drivers:** Silent message loss, no application-level acknowledgment, modal-state swallowing, cross-pane identity defects, evidence of unverifiable delivery
- **Supersedes / Superseded by:** Supersedes ADR-0005 (MessageBus stays separate)

## Context

The system currently routes cross-PM and cross-agent messages via two independent, **unreliable** channels:

### 1. **`tm sessions send` (trusty-mpm session messaging)**

- **Interface:** `SessionManager::send_input()` or CLI `tm sessions send <session-id> <text>`
- **Implementation:** Two literal `tmux send-keys` subprocess calls; blocking, returns `sent: true` when child processes exit 0
- **Problem:** Sends text to tmux but provides **no application-level acknowledgment**. A sent message may be:
  - Swallowed by an open trust dialog or provisioning modal (transport succeeds, application never sees it)
  - Typed into a different command-line context than intended (pane mismatch due to #3396 crossed `pane_id` records)
  - Accepted by a session in `Errored` or `Provisioning` state, with no verification that the session is ready to receive
- **Evidence:** Open issue #3396 (crossed `pane_id` records causing false refusals and wild sends); PR #3591 (interim readiness-gate fix for the reliability gap)

### 2. **Palace relay messaging (`memory_send_message` / `trusty-memory`)**

- **Interface:** `mcp__trusty-memory__memory_send_message` or CLI `tm memory send-message <recipient> <text>`
- **Implementation:** Stores message in the memory palace; recipient polls and drains it
- **Problem:** Both directions are **silent and unordered**:
  - A cross-project message sent 2026-07-18 went 30+ minutes with no delivery confirmation; recipient's own snapshot warned the channel was not delivering in either direction
  - No ordering guarantee or replay on re-connect
  - No way to distinguish "message never sent" from "sent but recipient never polled"
- **Evidence:** Session review on 2026-07-18; memory palace snapshot independently confirmed delivery failure

### Common root cause

Both channels lack:
1. **Explicit delivery acknowledgment:** "received", "processed", "queued" states
2. **Durability + replay:** messages lost on transport failure or recipient disconnect
3. **Modal-state gate:** no verification the recipient is ready
4. **Workstream-keyed addressing:** both use fragile identifiers (pane_id, session_id) instead of the stable orchestration hierarchy defined in ADR-0016

Together, this means **any message sent via either channel is unverifiable — even with "sent: true" confirmation, the sender has no proof the recipient got it or that it was processed**. This already caused a session to stand down work it owned, believing it had notified a peer that in fact never received the message.

### Why now: ADR-0005 explicitly forbade this

ADR-0005 §"Secondary design defaults" states: **"MessageBus stays separate... NOT a command/RPC bus."** The event bus was designed for telemetry (fan-out, lossy under lag, no delivery guarantee). 

That was correct for telemetry. But the owner has now decided the opposite for **messaging**: there will be ONE IPC channel for cross-PM and cross-agent coordination, and it MUST be built on the event bus, with delivery semantics. Leaving ADR-0005 live while building precisely what it forbids would mislead the next reader — hence supersession rather than silent contradiction.

## Decision

We will deprecate both `tm sessions send` and `memory_send_message` and consolidate all cross-PM, cross-agent messaging onto a **single, durable, explicitly-acknowledged IPC channel built on the event-driven control bus** (per ADR-0004, ADR-0005, and ADR-0016).

### Messaging Model (Owner-approved, 2026-07-21)

**Delivery semantics:** "Any received message should inject itself into the active workstream like a user message. All harnesses should support this with their normal message queueing."

**Architecture:**

1. **Bus owns:**
   - **Addressing:** keyed to durable **workstream/role identity** per ADR-0016 (Engineering Lead, PM, Assistant), never `pane_id` or `session_id`
   - **Durability + replay:** messages survive transport/recipient disconnect; resend on re-connect
   - **Explicit state machine:** Queued → Delivered → Processed (three acknowledgment levels per sender)

2. **Per-harness adapter owns last-mile delivery:**
   - **trusty-mpm PM:** tmux/pty injection via the common tmux module (consolidated under #2398/PR #2399); bus wraps tmux for the last mile
   - **trusty-code PM:** native message queue injection into Claude Code's user-message pipeline
   - **trusty-agents assistant:** native async queue injection

3. **Message shape (conceptual):**
   ```json
   {
     "message_id": "uuid-v4",
     "from": { "role": "Assistant", "workstream_id": "..." },
     "to": { "role": "PM", "workstream_id": "..." },
     "seq": 12345,
     "at": "2026-07-21T18:00:00Z",
     "text": "Start the integration test suite",
     "ack_mode": "Processed",
     "status": "Queued" | "Delivered" | "Processed"
   }
   ```

### Critical Nuance — tmux is not being discarded

The bus **wraps** tmux for trusty-mpm's last mile; it does not replace it. Tmux/pty text injection (`tmux send-keys`) is the correct and owned last-mile mechanism — the reliability gap is *above* the transport (no application-level gate or ack), not *below* it. The bus fixes that gap by:
- Verifying the PM session is ready before injecting
- Injecting into a known queue rather than blindly to a pane
- Confirming delivery at the harness level before returning Delivered/Processed

This approach keeps tmux as the last-mile transport for trusty-mpm but wraps it in reliability.

## Consequences

### Positive

- **Verifiable delivery:** each message carries a journey: Queued (accepted by bus) → Delivered (accepted by PM queue) → Processed (acted on by PM)
- **Single channel:** eliminates the cognitive load and duplicate implementation of two unreliable paths
- **Workstream-keyed addressing:** messages route to the right workstream even as pane IDs or session IDs drift (fixes the root cause of #3396 observed failures)
- **Standardizes message injection:** all three harnesses inject via the same event-bus contract, using each harness's native queue as the last mile
- **Replay on reconnect:** messages survive a PM restart or network hiccup; recipient re-subscribes and drains the backlog
- **Unifies telemetry + messaging:** ADR-0005's event bus becomes the unified control/data fabric, with message delivery as a special case of event acknowledgment

### Negative / Trade-offs

- **Execution gap:** the bus design was largely decided on 2026-07-18 (event/control bus architecture) and remains **unimplemented**. No code has landed since. This ADR records a decision; the work is future.
- **Phased deprecation required:** existing callers of `tm sessions send` and `memory_send_message` must migrate in phases:
  - **P0 (interim, shipped separately):** PR #3591 lands a readiness gate on `SessionManager::send_input()` to reduce modal-state swallowing, buying time for P1
  - **P1:** implement the unified messaging channel and bus adapters
  - **P2:** migrate existing callers (PMs, assistants) to the new channel
  - **P3:** remove the old paths from the codebase
- **Testing & verification cost:** the new channel must carry explicit acks; testing must verify all three states (Queued/Delivered/Processed) for every message path
- **Addressing scheme complexity:** must balance durable workstream identity (per ADR-0016) with the flexible session/pane model tmux requires; not a trivial mapping

### Neutral / Follow-up work

- **Epic #3052 and sub-issues BUS-1..BUS-13** (#3157–#3169, OPEN) track the implementation workstream. Zero code landed since 2026-07-18 team decision.
- **#3168 (BUS-7)** specifically scopes migrating `session_send` + `memory_send_message` to Message-class bus delivery
- **#3597** (filed 2026-07-21) captures the refined design decision from this conversation

## Related Decisions

Vetted against prior ADRs on 2026-07-21:

- **ADR-0004 (Three harnesses on shared event-driven common):** Extends. This ADR adopts ADR-0004's foundation as the transport for messaging; consistent with the three-harness coordination model.
- **ADR-0005 (Harness event bus):** Supersedes. ADR-0005 explicitly forbade a MessageBus on the event bus; this ADR reverses that decision, adopting a unified messaging channel on top of the event bus with explicit delivery semantics. **Action:** flip ADR-0005's status to "Superseded by 0019".
- **ADR-0016 (Orchestration Hierarchy: Engineering Lead / PM / Assistant):** Consistent. This ADR uses ADR-0016's role hierarchy (Assistant, Engineering Lead, PM) as its addressing model; the two decisions are complementary.
- **ADR-0018 (Loopback-only doctrine):** Consistent. Bus messaging is in-process (loopback); no new non-loopback binds.

No silent contradictions identified. Consistency verified.
