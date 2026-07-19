# 0016. Orchestration Hierarchy: Engineering Lead / PM / Assistant

- **Status:** Proposed
- **Date:** 2026-07-18
- **Scope:** Workspace-wide (trusty-agents, trusty-mpm, trusty-code)
- **Supersedes / Superseded by:** — (consolidates DOC-36 tm-manager-vision;
  to be reconciled with DOC-42 / PR #3006)

## Context

Three prior threads converged on the shape of multi-workstream orchestration
and left the role hierarchy underspecified in a single place:

1. **DOC-36 (tm-manager-vision, approved)** established a single supervisor
   role with an observe-and-verify, notify-only posture: it watches running
   work and surfaces problems, but does not act with user authority on its
   own. That approved role now maps onto the **Engineering Lead** defined
   below.

2. **DOC-42 (Engineering Lead / Virtual Twin architecture, PR #3006, open —
   not yet merged)** proposes a fuller architecture for the Engineering Lead
   role. This ADR decides the *hierarchy shape* — how Leads, PMs, and the
   Assistant relate and what scope each holds — that DOC-42's spec discusses.
   The two are to be reconciled: this ADR is the accepted shape; DOC-42's
   remaining architecture detail is follow-up work, not yet merged.

3. **Event-driven control bus decision (same day, 2026-07-18)** established
   that every action in the system emits an event, and that Leads act as
   subscribers over the workstreams they consolidate. The same bus is the
   transport for Assistant↔Lead and Lead↔PM messaging. A companion design
   pass is producing the full bus spec; this ADR only records the hierarchy
   the bus addressing scheme must serve.

Without a single decision recorded, "workstream," "project," and "lead" were
being used loosely across specs, and it was unclear whether the user or the
Assistant could talk to a Lead directly, and whether a Lead could ever act
with the user's authority. DOC-41 §5.5 already establishes that the
**Assistant** is the sole holder of user authority, and that invocation path
(who is talking to a role) is independent of authority (who may act on the
user's behalf). This ADR fixes the hierarchy those rules apply to.

## Decision

We adopt a three-role orchestration hierarchy:

1. **ENGINEERING LEAD** — manages a project and **consolidates multiple
   workstreams** (portfolio scope). A Lead is a consolidation and
   supervision point over the PMs and workstreams beneath it, inheriting
   DOC-36's notify-only, observe-and-verify posture: it monitors and
   surfaces problems, it does not carry user authority.

2. **PM** (a trusty-mpm session PM or a trusty-code PM) — owns **exactly
   one workstream**. Cardinality is N PMs per Lead; a PM never spans
   multiple workstreams and never manages another PM.

3. **ASSISTANT** — the single holder of user authority (per DOC-41 §5.5).
   The Assistant may converse with **multiple** Leads on the user's behalf,
   fanning out across projects. The user also retains a **direct**
   conversation path to any Lead. Per §5.5's invocation-independence
   principle, the user talking to a Lead directly does **not** confer user
   authority on that Lead — the Lead's authority posture is unchanged
   regardless of who invoked it.

Cardinality: **one Assistant : N Engineering Leads : M PMs per Lead**, where
each PM owns exactly one workstream.

The event-driven control bus (companion decision, same day) is the transport
for this hierarchy: Leads subscribe to events emitted by the workstreams
(PMs) they consolidate, and Assistant↔Lead / Lead↔PM messaging rides the same
bus. This ADR fixes the addressable roles and scope the bus's addressing
scheme must support; the bus spec itself is tracked separately.

## Consequences

### Positive

- **Single-workstream discipline for PMs** is now explicit: a PM that starts
  consolidating multiple workstreams is out of spec and should be split or
  promoted to Lead scope.
- **Leads are a clean consolidation/supervision point.** They sit at the
  portfolio level, subscribe to the bus over their workstreams, and inherit
  the already-approved DOC-36 notify-only posture rather than requiring a
  new authority model.
- **Authority never flows down.** Neither Leads nor PMs are ever
  user-authoritative, regardless of invocation path (Assistant-mediated or
  direct user conversation). This keeps DOC-41 §5.5's authority model
  intact even as the hierarchy grows to N Leads.
- **The user retains a direct escalation/inspection path** to any Lead
  without needing to route through the Assistant, useful for debugging or
  spot-checking a workstream.

### Negative / Trade-offs

- **Addressing and subscription need workstream/project scoping.** The bus
  must be able to route events and messages to the correct Lead (by project)
  and the correct PM (by workstream) — this is a requirement on the
  companion bus spec, not yet designed in detail.
- **DOC-36's original supervisor was singular; this decision generalizes it
  to N Leads.** DOC-36's monitoring-transport assumptions (built for one
  supervisor) need an amendment pass to confirm they hold at N-Lead scale.
- **DOC-42 is not yet merged.** Until PR #3006 lands and is reconciled
  against this ADR, DOC-42's architecture text and this ADR's hierarchy
  shape must be read together, with this ADR taking precedence on role
  cardinality and authority.

### Follow-ups

- Reconcile DOC-42 (PR #3006) against this ADR once it merges.
- Amend DOC-36 for multi-Lead monitoring transport.
- Land the companion event-driven control bus spec, using this ADR's role
  set (Assistant, Lead, PM) as its addressing model.

## Related

- **DOC-36** — tm-manager-vision (approved: single supervisor,
  observe-and-verify, notify-only) — superseded in scope (not content) by
  this ADR's N-Lead generalization.
- **DOC-41 §5.5** — Assistant as sole user-authority holder;
  invocation-independence of authority.
- **DOC-42 / PR #3006** — Engineering Lead / Virtual Twin architecture
  (open, to be reconciled).
- **Event-driven control bus decision (2026-07-18)** — companion design
  pass producing the bus spec that this hierarchy's addressing must serve.
