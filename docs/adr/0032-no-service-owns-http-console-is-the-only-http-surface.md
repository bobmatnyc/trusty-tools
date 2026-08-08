# 0032. No trusty-\* service owns an HTTP daemon; UDS is the inter-service transport; `trusty-console` is the only HTTP surface

- **Status:** Accepted — **Amended by
  [0034](0034-webhook-ingress-console-relays-over-uds-to-a-supervised-on-demand-process.md)**
  (2026-08-07 owner ruling "Console relays over UDS." closes the Open
  Question below: console terminates the webhook's HTTP request, verifies
  HMAC once, spools the payload durably before acknowledging, and relays
  over UDS to a console-supervised on-demand process). This ADR's Decision
  remains in force in full; its Context/Decision/Consequences are left as
  originally accepted per the ADR immutability rule (DOC-46 §4), and
  ADR-0034 is the record of the amendment.
  Its `0600` access-control premise is separately corrected by
  [#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099) — see the
  correction note in Consequences.
- **Date:** 2026-08-07
- **Accepted:** 2026-08-07 (owner ruling, verbatim below)
- **Scope:** Workspace-wide (every trusty-\* daemon that currently binds HTTP:
  trusty-search, trusty-memory, trusty-analyze, trusty-review, trusty-agents,
  trusty-mpm; trusty-console is the sole exception)
- **Reversibility Cost:** High — this is a topology change to every daemon
  that currently serves HTTP, not a policy refinement. Undoing it means
  re-adding HTTP listeners, origin guards, and port-discovery to six crates
  that will have removed them.
- **Decision Drivers:** owner directive on service topology; unblocks
  [#5028](https://github.com/bobmatnyc/trusty-tools/issues/5028)
  (undaemonizing trusty-analyze and trusty-review), scoped into milestone
  `tm 1.3.5` criterion (c)
- **Supersedes / Superseded by:** **Supersedes ADR-0018.** Ratifies
  ADR-0031 (Proposed) as the mechanism — see Related Decisions.

## Context

### The ruling, verbatim

The owner, 2026-08-07:

> "we've decided that nothing needs it's own, they can use udp for
> multi-client buffering, only 1 service needs http for user/external
> consumption"

Asked to confirm UDP vs. UDS, the owner confirmed:

> "UDS is right."

🔴 **The transport is UDS (Unix domain sockets), not UDP.** The owner's first
message said "udp"; the follow-up question and confirmation resolve it to
UDS. This ADR uses UDS throughout and the wording above is quoted only to
preserve the exact exchange.

### What this reverses

**ADR-0018 (Loopback-only doctrine, Accepted 2026-07-19)** explicitly
authorized every sibling daemon to run its own HTTP server, provided it
stayed loopback-bound. Its Decision section states, verbatim:

> "Sibling daemons (search, memory, analyze, review, agents, mpm) may each
> run their own loopback-only HTTP server for CLI use, MCP stdio-bridge
> proxying, and same-machine GUI clients — this is a supported pattern, not
> a violation."

That is precisely what this ruling removes. The owner's "only 1 service
needs http for user/external consumption" leaves no case in which a sibling
daemon owns an HTTP listener — not for CLI status, not for MCP stdio-bridge
proxying, not for an embedded UI. All of that traffic is now either UDS
(same-host, inter-service) or routed through the one HTTP service.

### What this ratifies

**ADR-0031 (Transport by purpose, Proposed 2026-08-06)** had already argued
for this shape — UDS for inter-crate same-host traffic, one shared HTTP
server (trusty-console) for everything external — but stated explicitly that
adoption was an **open owner evaluation, not a decision**: "The owner's words
were *'let's consider switching to UDS'* — an open evaluation... nothing here
commits to it." ADR-0031 also, at that time, still expected each daemon to
**keep** its own HTTP server for CLI status, health, metrics, embedded UIs,
and webhooks — its Decision explicitly lists "What stays HTTP by
definition" as including "every daemon shipping a UI keeps its HTTP server."

This ruling closes ADR-0031's open evaluation (UDS is adopted) and goes
further than ADR-0031 had proposed: it removes the "every daemon keeps
its own HTTP server for CLI/UI/webhook" carve-out entirely. Only the console
keeps HTTP. See Related Decisions for the precise scope of what changes
between ADR-0031's Proposed text and this ruling.

### Why now

This ruling unblocks 1.3.5 criterion (c) — undaemonizing trusty-analyze and
trusty-review — tracked as
[#5028](https://github.com/bobmatnyc/trusty-tools/issues/5028), which the
owner has scoped into milestone `tm 1.3.5`. Criterion (c) was previously
blocked as an open design question (which transport an on-demand,
non-always-listening process would use to receive calls); this ruling
answers that question for inter-service calls and scopes it as buildable
work.

## Decision

We will adopt the following topology, effective immediately as the target
architecture:

> **No trusty-\* service runs its own HTTP daemon.** Inter-service
> communication and multi-client buffering go over **UDS** (Unix domain
> sockets). **Exactly one service keeps HTTP**, for user- and
> external-facing consumption: **trusty-console**.

This is consistent with, and a direct continuation of, the owner's
2026-07-19 ruling (ADR-0018) that the console is the only non-loopback HTTP
surface — narrowed further here to say the console is the only HTTP surface
of any kind, loopback included.

Concretely:

- `trusty-search`, `trusty-memory`, `trusty-analyze`, `trusty-review`,
  `trusty-agents`, and `trusty-mpm` lose their own HTTP listeners. Their
  daemons continue to exist as shared, multi-client processes — the
  correctness case ADR-0031 already made for keeping a single-writer daemon
  per redb-backed store is unaffected by this ruling, which changes
  *transport*, not *process topology*.
- Every caller that currently reaches a sibling daemon over loopback
  HTTP — CLI commands, native MCP stdio bridges, embedded UIs,
  console-proxied traffic — moves to UDS for the inter-service hop.
- `trusty-console` remains the family's one HTTP surface: the only
  component external users, browsers, and off-host callers ever reach
  directly.

## Consequences

**Easier / positive:**

- Collapses six independent per-daemon HTTP stacks (bind address, origin
  guard, port-discovery file, CORS posture) into one: a UDS listener/dial
  module in `trusty-common`, mounted by every daemon, plus the single HTTP
  surface in `trusty-console`. This is the same "one shared abstraction, not
  N migrations" shape ADR-0031 already called for.
- Removes the per-daemon origin-guard and port-allocation surface area that
  `docs/reference/threat-model.md` exists to audit — a `0600`-permissioned
  socket in a user-owned directory replaces a loopback TCP port reachable by
  any local process, which is a stronger guarantee, not a weaker one.

> 🔴 **Correction (#5099, 2026-08-07).** The `0600`-permissioned socket cited
> here was not implemented anywhere in the workspace when this ADR was written;
> see ADR-0031's matching correction note. It became true in
> [#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099). Note also that
> the "UDS listener/dial module in `trusty-common`, mounted by every daemon"
> assumed above still does not exist as a *transport*; #5099 supplied only its
> security layer, and [#5089](https://github.com/bobmatnyc/trusty-tools/issues/5089)
> step 1 builds the framing and dialing on top.
- Unblocks [#5028](https://github.com/bobmatnyc/trusty-tools/issues/5028):
  trusty-analyze and trusty-review can now be built as on-demand processes
  rather than always-listening daemons, because their inter-service surface
  no longer requires an HTTP listener to be up and bound.
- Gives ADR-0031's classification test ("is the caller outside this host, or
  another crate on it?") a settled mechanism instead of an open one — the
  test itself needed no change, only its "if adopted" qualifier is removed.

**Harder / negative / trade-offs:**

- **Every daemon's embedded UI loses its direct HTTP path.**
  `trusty-search`, `trusty-memory`, and `trusty-analyze` each serve an
  embedded SPA over their own HTTP listener today
  (`docs/reference/threat-model.md`'s "Own UI" column). Browser clients
  cannot reach a UDS socket, so each embedded UI's requests must be served
  by, or proxied through, the console instead. This is strictly more work
  than ADR-0031 had scoped, which still let each daemon serve its own UI
  directly.
- **CLI status/health commands** (`trusty-search status`, `tm status`, etc.)
  that currently do a quick loopback HTTP `GET` must move to a UDS dial —
  mechanical, but touches every CLI entry point across six crates.
- **`docs/reference/threat-model.md`'s per-daemon inventory goes stale.**
  Its bind/guard/console-proxy-allowlist table is built entirely on ADR-0018
  premises (which daemons may bind loopback HTTP and how they're guarded).
  With no sibling daemon binding HTTP at all, most of that table's columns
  no longer apply. This ADR does not attempt to re-derive the table — see
  Open Questions and the follow-up note below.
- **This is a High reversibility-cost, workspace-wide migration**, not a
  policy clarification. It touches every daemon's listener setup, every
  CLI's client code, every native MCP stdio bridge, and every embedded UI's
  data-fetch layer.

## Open Questions

🔴 **This ruling does not answer how webhook ingress reaches an on-demand
process.** Two existing surfaces assume an always-listening HTTP server:

- `trusty-review` has a GitHub webhook (HMAC-verified,
  `docs/reference/threat-model.md`'s trusty-review row).
- `trusty-analyze` has a direct `POST /webhooks/github`
  (`crates/trusty-analyze/src/service/routes.rs`), already flagged as an
  ADR-0018-era known gap.

Both are external senders (GitHub) delivering to a process this ruling says
should not run its own HTTP listener. Routing webhook delivery through the
single console HTTP surface — console receives the webhook and relays it
inward over UDS — is the obvious candidate shape, and is consistent with
ADR-0017's already-Proposed shared `/api/webhooks/{source}` design. **The
owner did not say this.** This ADR records it as the open question it is,
not as a decision:

> **Open: how does an external webhook reach a service that no longer runs
> an HTTP listener, when that service is not always resident to receive a
> relayed call?** If the target is on-demand (per #5028's undaemonizing
> goal), console relaying over UDS requires either the target process to be
> already running, or a spawn-on-delivery mechanism that does not yet exist
> in any ADR. This is exactly the design question #5028 needs answered
> before trusty-analyze/trusty-review can be undaemonized, and it is
> explicitly **not resolved by this ADR.**

Whoever picks up #5028 or lands ADR-0017 needs to resolve this before
webhook-bearing daemons can lose their HTTP listeners in practice — until
then, `trusty-analyze` and `trusty-review`'s webhook routes are a known
carve-out this ADR does not close.

## Follow-up work

- `docs/reference/threat-model.md`'s per-daemon bind/guard/proxy inventory
  needs revising to reflect that no sibling daemon binds HTTP. That
  re-derivation is implementation work, gated on the open question above
  (webhook ingress) being resolved, and is out of scope for this ADR. This
  ADR only flags the document as superseded-pending-revision (see the note
  added to that file).
- ADR-0031 should be moved from Proposed to Accepted, or folded into this
  ADR's text, now that its central open question (adopt UDS?) is settled.
  **Recommendation, not a unilateral rewrite:** this ADR does not change
  ADR-0031's Status field itself — see Related Decisions.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior ADRs on 2026-08-07:

- **ADR-0018 (Loopback-only doctrine):** **Superseded.** ADR-0018's core
  allowance — sibling daemons may each run their own loopback-only HTTP
  server — is exactly what this ruling removes. ADR-0018's Status field is
  updated to `Superseded by 0032` in the same change as this ADR. Its
  Context/Decision/Consequences sections are left as originally accepted
  per the ADR immutability rule (DOC-46 §4); this ADR is the record of the
  reversal, not an edit to that history. ADR-0018's relationship to
  ADR-0011 (which it amended) is unaffected — ADR-0011 stays `Amended by
  0018`, since that amendment's content is unrelated to the clause being
  superseded here.

- **ADR-0031 (Transport by purpose: UDS inter-crate, HTTP external only,
  Proposed):** **Ratifies, and narrows.** ADR-0031 already argued for UDS as
  the inter-crate transport and named its adoption as the open question this
  ruling now answers: "yes, adopt UDS." Where this ADR **narrows** ADR-0031's
  own Proposed text: ADR-0031 still expected every daemon to keep its own
  HTTP server for CLI status, embedded UIs, and (for analyze/review)
  webhooks — its "What stays HTTP by definition" list. This ruling removes
  that carve-out: only trusty-console keeps HTTP, full stop. ADR-0031's
  classification test ("is the caller outside this host, or another crate on
  it?") and its all-or-nothing UDS scope condition both stand unchanged and
  are the mechanism this ADR adopts.

  **Recommendation on ADR-0031's Status:** move it to `Accepted` (its central
  open question is now resolved) with a short amendment note pointing at
  this ADR for the narrowed scope, rather than leaving it Proposed
  indefinitely or duplicating its content here. This ADR does not make that
  edit — flagging it is as far as this document goes, per the instruction
  not to unilaterally rewrite another ADR's status without saying so.

- **ADR-0017 (Shared webhook ingress via trusty-console + Tailscale Funnel,
  Proposed):** **Consistent / Extends — and now load-bearing.** ADR-0017's
  single `/api/webhooks/{source}` endpoint, reverse-proxied by console, was
  already the obvious shape for webhook ingress under a console-only HTTP
  topology. This ADR does not adopt ADR-0017's design as settled — see Open
  Questions — but notes that ADR-0017 becomes more directly relevant now
  that no sibling daemon can host its own webhook route at all.

- **ADR-0011 (`tctl` owns service lifecycle; console owns the single HTTP
  surface):** **Consistent.** Already `Amended by 0018`; unaffected by this
  ADR. ADR-0011's original "HTTP exactly once" intent is, in effect, what
  this ruling restores in full — this ADR is closer to ADR-0011's original
  text than ADR-0018 was.

- **ADR-0014 (Ship full native MCP support):** **Consistent.** ADR-0014
  consolidated the MCP framework in `trusty-common`; this ADR changes the
  transport those servers' stdio bridges use to reach their daemon (UDS
  instead of an HTTP client), not the MCP framework itself.

No conflicts with any other Accepted ADR. Summary: supersedes ADR-0018,
ratifies and narrows ADR-0031, and leaves ADR-0011/ADR-0014/ADR-0017
consistent.
