# 0021. Slack inbound = hybrid gateway+eventstream

- **Status:** Accepted
- **Date:** 2026-07-24
- **Scope:** crate `trusty-agents` (`slack::{mod, handlers}`, `listeners::store`, `listeners::poll`, `listeners::wake`)
- **Reversibility Cost:** Low — additive only; no schema break, no removed code path, no data migration
- **Decision Drivers:** owner directive to retire the custom Duetto `cto_bot` Slack app onto the native connector without regressing the working conversational bot; validate the just-merged listener/wake engine (#3838) on a second real connector after Gmail; avoid double-dispatch (one inbound message must produce exactly one reply)
- **Supersedes / Superseded by:** none

## Context

Epic #3852 (owner directive, 2026-07-24) calls for replacing the custom Duetto `cto_bot` Slack app (`A0AMPRM4W0J`) with trusty-agents' native Slack integration. Two architectures existed side by side going into this work, and #3852's own epic body sketches the listener-based one without reconciling it against the Socket-Mode gateway that already ships and works:

1. **The Socket-Mode gateway** (`crates/trusty-agents/src/slack/{mod,handlers,rbac,pairing,relay,format}.rs`, tracked since #418/#480/#481). A long-running WebSocket connection (`run_slack_bot`) that ACKs every Slack envelope immediately and dispatches plain messages synchronously to `ctrl::run_pm_task_with_persona` (`handlers.rs::handle_message`), replying via its own `chat.postMessage` (`handlers.rs::post_message`/`send_long_message`). RBAC (per-user `ServiceTier` + persona allow-list, `slack::rbac`) and pairing (`slack::pairing`) are fully built out. This is the ONLY Slack transport this codebase has ever shipped; it is what makes the bot answer a DM today.
2. **The listener/wake engine** (`crates/trusty-agents/src/listeners/{config,poll,store,wake}.rs`, #3838/#3820, DOC-54 SPEC-AGENTS-04/06). A generic two-stage-filtered eventstream: a connector's poll loop appends every qualifying event to an append-only `EventStore` (`~/.trusty-agents/events/events.jsonl`), mirrors it live via `Event::ListenerEventReceived` onto the harness SSE bus for the Events pane, and — for events that ALSO pass a per-agent `[[listeners]]` stage-two filter — wakes a bound agent (`wake::wake_bound_agents`), rate-limited to at most one wake per poll cycle. This shipped for Gmail only; DOC-54 §7 (SPEC-AGENTS-06) documents Gmail and Google Calendar exclusively — there is no Slack section.

Building Slack purely as a THIRD listener/wake connector (as #3852's epic body sketches, mirroring Gmail's poll-and-wake shape) would mean either (a) replacing the working Socket-Mode gateway with an as-yet-unwritten Slack poll/webhook ingestion path, discarding the pairing/RBAC/persona-switch machinery that already exists and is exercised by 26 passing unit tests, or (b) running both transports independently with no relationship between them, risking a Slack DM being answered TWICE — once by direct gateway dispatch, once by a stage-two wake reacting to the same message via the eventstream.

Building Slack purely as the EXISTING gateway with no eventstream integration (the status quo before this ADR) would mean the Events pane — the harness's one cross-connector visibility surface (§8.6) — never shows Slack traffic at all, and a future stage-two wake binding (for channels/threads the direct gateway doesn't handle conversationally, e.g. a channel mention rather than a DM) would need its own from-scratch ingestion path duplicating what `EventStore`/`ListenerEventReceived` already provide.

## Decision

We will run Slack as a **hybrid**: the Socket-Mode gateway remains the transport and the sole reply path, and it ADDITIONALLY mirrors every inbound plain message onto the generic listener eventstream.

Concretely:

1. **Transport and reply path unchanged.** `run_slack_bot` / `open_socket_and_run` / `dispatch_envelope` / `handle_message`'s dispatch to `ctrl::run_pm_task_with_persona` and reply via `chat.postMessage` are not modified in shape. This is a deliberate rejection of "rebuild Slack as a poll/webhook connector" — the gateway already works, is fully RBAC/pairing-gated, and losing it would regress an owner-facing production bot mid-migration.
2. **Additive eventstream mirror.** `handlers::record_listener_event` (new) appends a `StoredEvent` (`provider: "slack"`, `listener_id: "slack"`, `event_type: "message.<channel_type>"` e.g. `message.im`/`message.mpim`) to the SAME `EventStore` Gmail uses, then consults `EventStore::is_event_type_included` and publishes `Event::ListenerEventReceived` — reusing the identical generic SSE variant Gmail's poll loop emits, so the Events pane needs zero Slack-specific rendering code. This runs BEFORE the RBAC/dispatch branch in `handle_message`, so it captures every message in a paired channel regardless of whether the sender is a known RBAC user.
3. **No wake binding wired.** `listeners::wake::wake_bound_agents` is deliberately NOT called from the Slack path in this change. The direct dispatch in step 1 already answers the message; invoking a stage-two wake on the SAME event would produce two replies to one DM. `record_listener_event`'s `StoredEvent` shape and the generic `ListenerEventReceived` mirror are structured so a FUTURE stage-two binding (for a use case the direct gateway doesn't already answer — e.g. a passive channel-mention watch) could consume these events without rework; wiring one is explicitly deferred, not designed away.
4. **#3065's channel→agent routing is satisfied by RBAC, not left open.** #3065 asked how a Slack channel maps to which agent replies. Under this hybrid, that question is already answered for the DM surface by `slack::rbac::SlackRbacConfig` (per-user tier + `default_persona`, with `/slack-switch` gated by an allow-list) — the routing mechanism #3065 wanted already exists and predates this ADR. #3065 remains open ONLY for a future multi-agent CHANNEL (not per-user DM) routing scheme, which is a distinct, not-yet-designed problem this ADR does not attempt to solve.
5. **Spec gap flagged, not fixed here.** DOC-54 §7 (SPEC-AGENTS-06 — Eventstream Listeners) documents Gmail and Google Calendar only. This ADR records that Slack now participates in the SAME `EventStore`/`ListenerEventReceived` contract as those two connectors (append-then-filter order identical to `listeners::poll::poll_once`, see `handlers::record_listener_event`'s doc comment), but a proper "Slack connector" subsection in DOC-54 §7 — covering the hybrid shape, the `message.<channel_type>` event-type convention, and the deferred-wake posture — is a follow-up spec-editing task, out of scope for this PR.

## Consequences

### Positive

- Zero regression to the working, RBAC/pairing-gated Socket-Mode gateway during the cutover — the owner's production bot keeps answering DMs exactly as it does today, satisfying the "side-by-side validation, old bot as fallback" cutover strategy the epic calls for.
- The Events pane gains live Slack visibility for free: it consumes the SAME `Event::ListenerEventReceived` variant and `EventStore` file Gmail already populates, so #3818 (Events pane) needs no Slack-specific code path.
- `record_listener_event`'s append-then-filter order is a direct structural copy of `listeners::poll::poll_once`'s (append unconditionally, check `is_event_type_included` for the CURRENT state, publish with that flag) — a reader who understands the Gmail path already understands the Slack one; no new mental model.
- Future stage-two wake work (a genuinely new use case, e.g. reacting to channel mentions the gateway doesn't already answer) can bind to `event_type = "message.im"`/`"message.mpim"` in an agent's `[[listeners]]` exactly like Gmail's `event_types = ["message.received"]`, with zero changes to the append path.

### Negative / Trade-offs

- **Two code paths now touch the same `EventStore`/event bus for two different reasons** (Gmail: poll-and-optionally-wake; Slack: dispatch-and-always-mirror-never-wake-from-here). A future engineer adding a THIRD connector must read both to understand which posture applies, rather than there being one obvious pattern; this ADR is the record of that split and why it exists.
- **`listener_id: "slack"` is a single fixed constant**, unlike Gmail's per-mailbox `ListenerConfig.name` — there is no per-workspace Slack listener config in this slice (the gateway is one process-wide Socket-Mode connection, not N configured listeners). If trusty-agents ever needs to run against multiple Slack workspaces from one process, `SLACK_LISTENER_ID` will need to become workspace-scoped; deferred until that need is real.
- **No wake binding means the eventstream mirror is currently read-only/observational** for Slack — it feeds the Events pane and nothing else. This is intentional (see Decision point 3) but is a real capability gap relative to Gmail (which DOES wake agents) until a genuinely new, non-double-dispatching use case justifies wiring one.
- **DOC-54 §7 spec gap remains open** — this ADR documents the decision in code-adjacent form but does not edit the spec. Tracked as follow-up, not fixed here (see Decision point 5).

### Neutral / Follow-up work

- Add a Slack subsection to DOC-54 §7 (SPEC-AGENTS-06) documenting the hybrid shape and the `message.<channel_type>` event-type convention.
- If/when a genuine non-DM use case appears (channel-mention watching, a passive digest agent, etc.), design a stage-two `[[listeners]]` binding for `connector = "slack"` that is provably exclusive of the direct-dispatch path (e.g. scoped to channels the gateway doesn't already handle conversationally) before wiring `wake::wake_bound_agents` into `record_listener_event`'s call site.
- #3065 (channel→agent routing) stays open for multi-agent CHANNEL routing specifically; the DM-surface question it originally raised is closed by the existing RBAC table (Decision point 4).

## Related Decisions

Vetted against prior ADRs on 2026-07-24:

- **ADR-0004 (Three harnesses, shared event-driven trusty-common foundation):** Consistent. This ADR's eventstream mirror rides the SAME event-driven foundation (the process-global broadcast bus, `crate::events::publish`) ADR-0004 establishes as the cross-harness pattern; no new bus or transport is introduced.
- **ADR-0005 (Shared HarnessEvent envelope + process-global event bus) / ADR-0019 (Unified IPC messaging on the event bus):** Consistent (and, per ADR-0005's own status, superseded-in-spirit by ADR-0019's unification, which this ADR does not touch). `Event::ListenerEventReceived` is an existing variant on the SAME unified bus ADR-0019 describes; this ADR adds a new PRODUCER (the Slack gateway) of an existing event type, not a new bus, envelope, or delivery mechanism.
- **ADR-0015 (Unified agent composition model) / ADR-0016 (Orchestration Hierarchy):** Consistent. Neither the gateway's persona/RBAC dispatch nor the new eventstream mirror changes how agents are composed or how orchestration authority flows; `default_persona`/`/slack-switch` routing predates and is unaffected by this ADR.
- **ADR-0018 (Loopback-only doctrine):** Consistent. The Socket-Mode gateway is an OUTBOUND WebSocket client (no inbound listening socket at all), and the new eventstream mirror is in-process (`crate::events::publish`) — neither opens or changes any network-facing bind, loopback or otherwise.

No conflicts identified.
