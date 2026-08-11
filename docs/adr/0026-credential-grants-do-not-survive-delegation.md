# 0026. A Credential Grant Does Not Survive Delegation

- **Status:** Accepted
- **Acceptance:** The authority model recorded here is an **owner decision
  given on 2026-08-01**, not a proposal. Its normative encoding is
  [DOC-45](../specs/DOC-45-credential-authority-model.md) §6
  (`SPEC-CREDAUTH-04~draft`); nothing in it is implemented yet.
- **Date:** 2026-08-01
- **Scope:** Crate `trusty-common` (the credential authority: `Principal`,
  `CredentialRef`, ACL, revocation, audit); crate `trusty-agents` (assistant and
  sub-agent principals, the `delegate_to_agent` boundary). Cross-product by owner
  decision — *"#4040 yes for agents and code"* — so `trusty-code` is in scope
  through its service principals.
- **Reversibility Cost:** **High.** The decision fixes the granularity of
  `Principal` and the signature of every resolution call. Reversing it later means
  re-issuing every grant under a different key and re-auditing every delegation
  path; and because the rejected alternatives are strictly *wider*, a reversal
  would silently widen reach that operators had reason to believe was bounded.
- **Decision Drivers:** The trust boundary moves without closing under delegation
  (#4417, #4479, #4439 all independently identified this); auditability of an
  implicit versus an explicit grant; the untrusted-input surface of L0 assistants
  that ingest Gmail/Drive/Calendar; consistency with the already-ratified
  fail-closed posture of `delegate_allowed` and `ASSISTANT_REACHABLE_SUBAGENTS`
  (ADR-0024 decision 4); and the owner's virtual-twin authority principle recorded
  in ADR-0024 — *each assistant takes authority over its own actions, and that
  authority is not transferable.*
- **Supersedes / Superseded by:** — Supersedes nothing. This is the first ADR to
  record a credential-authority decision; the two prior attempts to settle the
  adjacent at-rest question (#3066, #3076) were both closed `NOT_PLANNED` on
  2026-07-28 without an answer and produced no decision record.

## Context

### 1. The question, and why it blocked five issues

Epic #4040 ("unified credential authority, delivery, and audit") had five
downstream consumers waiting — #4417, #4479, #4439, #4478, and the OKG Sources
work (#4531 / DOC-63) — and every one of them was blocked not on a library but on
a decision. Its child #4563 carried four owner questions. The fourth was the one
that gated the rest:

> **Q4 — Does a credential grant survive delegation, or must a sub-agent hold its
> own?**
>
> #4417 and #4439 both hinge on this and neither answers it. If an assistant's
> grant flows through to a `version-control` or Computer Use sub-agent, delegation
> moves the trust boundary without closing it — the exact shape both issues flag.
> If the sub-agent must hold its own grant, that is a different and larger design.

The shape both issues flag, stated concretely by #4417: an L0-tier assistant such
as `izzie` ingests untrusted third-party content (Gmail, Drive, Calendar — PR
#4222). The stopgap removes its direct git tools and routes git through a
sub-agent. But if `izzie` can simply *ask* that sub-agent to run a git operation
using `izzie`'s own credential, the credential is still reachable from a persona
fed by untrusted input, one hop further out. #4479 states the same conclusion
independently: *"routing git through a sub-agent MOVES the trust boundary without
CLOSING it."*

### 2. Verified current state

On `origin/main` (`11ef5c27`), production source only:

- **There is no principal concept in the credential path at all.**
  `resolve_key(provider)`
  (`crates/trusty-common/src/inference/credentials/resolver.rs:79`) takes a
  provider name and nothing else. Any code in any crate that can call it gets any
  credential the process can see. That is the entire access-control model. There
  is therefore nothing today that *could* survive delegation, because there is
  nothing that is scoped in the first place — the question is about the model
  being built, not about a behaviour being changed.
- **The adjacent reachability model is already fail-closed, and already
  server-owned.** `ASSISTANT_REACHABLE_SUBAGENTS`
  (`crates/trusty-agents/src/agents/delegation.rs:78`) is a code-owned floor a
  persona's `[subagents].delegate_allowed` may only *narrow*, never widen (ADR-0024
  decision 4, ratified 2026-07-29, implemented PR #4314). Its resolution doc states
  the posture in one line: *"`None` reaches nothing — fail-closed"*
  (`crates/trusty-agents/src/runtime/tool_registry.rs:549`). Any credential answer
  that were *not* fail-closed would sit beside a delegation model that is.
- **`user_authority` exists and is enforced** in 10 source files
  (`crates/trusty-agents/src/agents/permissions.rs`,
  `api/server/agent_permissions.rs`, `tools/cross_product.rs`) — but it gates
  *tools*, never credentials. The seam #3076 wanted for a `user.*` credential
  namespace is real and still open.

### 3. The three candidate models

| Model | Behaviour | |
|---|---|---|
| **inherit-unchanged** | A sub-agent resolves with its delegator's grants, unmodified. | Simplest; nothing to configure. |
| **inherit-narrowed** | A sub-agent resolves with a subset of its delegator's grants, derived at the hop. | Intuitively "safe"; the shape #4566's original acceptance criteria assumed. |
| **own-grant (chosen)** | A sub-agent is a distinct principal and resolves only against grants issued to it. | Fail-closed; more configuration. |

## Decision

**We will make a credential grant non-transitive. A sub-agent holds its own
grant, checked against its own principal. An assistant cannot lend the reach it
holds.**

Recorded verbatim from the owner, 2026-08-01, on #4563 and #4040:

> A sub-agent must hold its own grant, checked against its own principal — an
> assistant cannot lend the reach it holds. This is fail-closed, consistent with
> `delegate_allowed` resolving absent config to the empty set.

Four consequences follow, and are normative in DOC-45 §6:

1. **`SubAgent { name, delegator }` is a distinct principal** from
   `Assistant(delegator)` (DOC-45 `C-1.1`, `C-4.2`). It is keyed on the *pair*, so
   `izzie`'s `version-control` and `cto-assistant`'s `version-control` are
   separately grantable and separately revocable.
2. **Resolution is always evaluated against the calling principal** — no delegator
   fallback, no ambient credential context, no "resolve as my parent" parameter
   (`C-4.1`). A sub-agent with no grant of its own receives `Denied`, not a
   fallback (`C-4.3`).
3. **A resolved secret may not cross a delegation hop** (`C-4.4`) — not as a task
   parameter, a prompt fragment, a `HandoffContext` field, an environment overlay,
   or a file. Without this clause the decision is bypassed in one line
   (`delegate_to_agent(task: "run gh with token sk-…")`) and becomes theatre.
4. **There is no delegator-derived ceiling** (`C-4.5`). A sub-agent's reach is not
   intersected with its delegator's. See "Consequences" — this is the clause most
   likely to be added by a well-meaning implementer, and adding it would break the
   directive it appears to protect.

### Rejected alternatives, and why

**inherit-unchanged — rejected.** Recorded reason: *it would flow an
untrusted-input persona's reach straight to whatever it delegates to.* This is
precisely the shape #4417 and #4479 filed against. It also makes the trust
boundary invisible: nothing in any configuration would record that a sub-agent can
reach a credential, because its reach would be a runtime property of whoever
happened to call it.

**inherit-narrowed — rejected.** Recorded reason: *implicit inheritance is harder
to audit than an explicit per-principal grant.* The narrowing is a derivation
performed at the hop; answering "what can this sub-agent reach?" requires
simulating every delegation path that can reach it rather than reading a list.
It is also the model that degrades worst under change: adding a grant to an
assistant silently widens every sub-agent it can delegate to.

## Consequences

### Positive

- **The trust boundary closes rather than moves.** #4417's, #4479's, and #4439's
  shared blocker is resolved by a single property, which is why #4563 alone
  unblocks all three.
- **Reach is a readable list.** "What can this sub-agent reach?" is answered by
  reading its grants, not by simulating delegation paths. This is the auditability
  the owner cited against inherit-narrowed.
- **Revocation is precise.** Revoking `izzie`'s `version-control` GitHub grant does
  not touch `cto-assistant`'s, because the principals are distinct pairs.
- **It composes with what is already ratified.** Fail-closed matches
  `delegate_allowed`'s `None`-reaches-nothing posture and ADR-0024 decision 4's
  server-owned floor; DOC-45 `C-3.7` applies the same floor-that-config-can-only-
  narrow shape to credentials.

### Negative — stated honestly

- **More configuration.** Routing git through `version-control` (#4417 / #4479)
  now requires the operator to grant that sub-agent its own GitHub credential;
  Computer Use (#4439) likewise. DOC-45 `C-4.6` requires the first denial to name
  the principal and the ref so the operator's next action is one grant command,
  not a debugging session — but the burden is real and is the price of the
  auditability above.
- **A configuration cliff on first use.** Every routed capability will fail closed
  the first time until granted. This is correct and it will be reported as a bug.
- **A sub-agent may hold reach its delegator does not.** That is a genuinely
  unusual property and it is deliberate — see below.

### The trap: do not add a delegator-derived ceiling

A reviewer or implementer will observe that `effective(subagent) ⊆
effective(delegator)` looks like free safety: a ceiling only ever denies, so it
cannot widen anything, and it is not the inheritance the owner rejected.

**Adding it would make #4417 unimplementable.** The entire point of routing git
through `version-control` is that *the assistant need not hold git reach*. If the
sub-agent's GitHub grant were capped by the assistant's — empty, by design — the
routed call would resolve to nothing. The ceiling is rejected here on its own
merits, and DOC-45 `C-4.5` states it normatively so it cannot be reintroduced as
a "hardening" change.

### Follow-up required

- **#4566's acceptance criteria are stale.** Its bullet *"Delegation narrows: a
  sub-agent principal derived from a delegator resolves a strict subset;
  property-tested"* was written before this decision and describes
  **inherit-narrowed**. It must be replaced by a test of `C-4.2` + `C-4.3` (a
  sub-agent resolves `Denied` for a credential its delegator holds) and of
  `C-4.4`.
- Two owner questions remain open and block parts of DOC-45 — audit granularity
  and topology (Q-A, blocking #4567) and whether two assistant instances share a
  credential namespace (Q-B, blocking `Principal`'s `Assistant` variant in #4566).
  Neither affects this decision: non-transitivity holds at either granularity.

## Related Decisions

**Vetting required before acceptance.** Vetted against `docs/adr/INDEX.md` and the
prior decisions on 2026-08-01:

- **ADR-0024 (Assistants are L0 delegators; sub-agents are in-process,
  single-edge leaves that never delegate):** **Extends.** ADR-0024 fixes the
  delegation *topology* and, in its decision 4, the server-owned floor over the
  reachable sub-agent set. This ADR adds the *credential* dimension over that same
  topology: reachability says an assistant may call a sub-agent; this decision says
  doing so confers no credential. ADR-0024's underlying rationale — the owner's
  virtual-twin authority principle, that *"each assistant must take authority over
  its own actions, and that authority is not transferable between assistants"* — is
  the same principle applied one tier down. No conflict; DOC-45 `C-3.7` deliberately
  reuses ADR-0024 decision 4's narrow-only composition rather than inventing a
  second shape.
- **ADR-0023 (Worktree authority: existence vs ownership):** **Consistent.** Both
  separate "the resource exists and is visible" from "this actor may act on it".
  No overlap in mechanism.
- **ADR-0018 (Loopback-only doctrine):** **Consistent, and depended upon.** DOC-45
  `C-8.4`'s remote/fleet delivery uses the existing authenticated channel rather
  than opening a new listener, and `C-7.12` defaults the audit sink to local-only
  for the same reason.
- **ADR-0017 (Shared ingress via console Tailscale funnel):** **Consistent, and
  its forward reference is now resolvable.** ADR-0017 says *"Webhook signing
  secrets follow DOC-45's credential model"* — written when DOC-45 did not exist.
  It now does; the referenced model is DOC-45 §5 and §10. `TRUSTY_WEBHOOK_SECRET`
  / `GITHUB_WEBHOOK_SECRET` are among the 13 credential names #4564 must register.
- **ADR-0012 (Per-instance GUID and marker-file identity):** **Consistent.**
  Establishes that instance identity is derived from a marker the instance does not
  author, which is the same property DOC-45 `C-1.2` requires of a `Principal` —
  derived by the authority, never self-declared.
- **ADR-0015 (Three-product agent composition model):** **Consistent.** Supports
  this decision's cross-product scope: a credential authority that lives in
  `trusty-common` and is consumed by all three products is the composition model
  applied to secrets. `trusty-agents-common` is ruled out as a home by the
  dependency graph (zero `trusty-*` deps, a deliberate leaf breaking a cargo
  cycle), not by preference.
- **ADR-0002 (Single install convention)** and the repo's common-entry-point
  rule: **Consistent, and reinforced.** DOC-45 `C-3.3` requires exactly one
  resolution entry point, and DOC-63 `S-5.2` already declares a second credential
  mechanism a defect. This ADR adds no parallel path.

No prior decision is superseded or in conflict.
