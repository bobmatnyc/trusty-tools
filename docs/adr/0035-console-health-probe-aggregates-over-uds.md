# 0035. `trusty-console` gains a fleet health probe that aggregates over UDS from every trusty-\* service; CLIs stop dialing daemons point-to-point — amends ADR-0032

- **Status:** Accepted
- **Date:** 2026-08-08
- **Accepted:** 2026-08-08 (owner ruling, verbatim below)
- **Scope:** Workspace-wide — concretely `trusty-console` and the CLI
  status/health entry points across the same six crates ADR-0032's
  Consequences section named (`trusty-search`, `trusty-memory`,
  `trusty-analyze`, `trusty-review`, `trusty-agents`, `trusty-mpm`)
- **Reversibility Cost:** Medium — the per-CLI UDS-dial mechanism this ADR
  replaces was never implemented (ADR-0032 landed 2026-08-07; this amends it
  one day later), so reverting mostly costs re-deciding a mechanism, not
  un-migrating six crates' worth of shipped dial code.
- **Decision Drivers:** owner ruling 2026-08-08; the common-entry-point rule
  in `CLAUDE.md` (one implementation per shared capability, not N); the
  duplication this rule exists to prevent, already visible in-tree between
  `crates/trusty-installer/src/commands/probe_http.rs` and
  `trusty-common`'s `health_probe.rs` / `daemon_guard.rs` / `daemon_addr.rs`;
  [#4925](https://github.com/bobmatnyc/trusty-tools/issues/4925) (`tctl`
  must say something useful when a service is stopped)
- **Supersedes / Superseded by:** **Amends ADR-0032** (its second amendment,
  after [0034](0034-webhook-ingress-console-relays-over-uds-to-a-supervised-on-demand-process.md)).
  ADR-0032's Decision — no sibling daemon runs its own HTTP server,
  inter-service traffic is UDS, console is the only HTTP surface — is
  untouched and remains in force. This ADR replaces one specific mechanism
  named in ADR-0032's Consequences section (a per-CLI UDS dial for
  status/health) with a console-side aggregator. ADR-0032's Status is
  updated to record both amendments; its Context/Decision/Consequences are
  left as originally accepted per the ADR immutability rule (DOC-46 §4).

## Context

### The ruling, verbatim

The owner, 2026-08-08:

> "Console should have a health probe that probes all the UDS services."

### What this replaces

ADR-0032's Consequences section, under "Harder / negative / trade-offs,"
currently states:

> "CLI status/health commands (`trusty-search status`, `tm status`, etc.)
> that currently do a quick loopback HTTP GET must move to a UDS dial —
> mechanical, but touches every CLI entry point across six crates."

That describes a **per-CLI, point-to-point** mechanism: each of `tm status`,
`trusty-search status`, and the rest independently dials its own daemon's
UDS socket, decides its own timeout, and implements its own failure
taxonomy. This ruling replaces that shape with an **aggregation** model:
`trusty-console` grows a health probe that fans out over UDS to every
trusty-\* service and aggregates the results; CLIs stop probing daemons
directly and instead consume console's aggregated output.

### Why this follows from ADR-0032 rather than reopening it

ADR-0032 already made `trusty-console` the family's one HTTP surface. Making
it the one health aggregator is the same principle — one component, not six
— applied to a second capability. The alternative that ADR-0032's
Consequences section had implicitly left standing was six crates each
independently implementing a UDS dial, a timeout policy, and a failure
taxonomy for status/health. That is exactly the duplication the
common-entry-point rule in `CLAUDE.md` exists to prevent, and this is not a
hypothetical risk: `crates/trusty-installer/src/commands/probe_http.rs`
already exists as a separate implementation alongside `trusty-common`'s
`health_probe.rs`, `daemon_guard.rs`, and `daemon_addr.rs` — the drift this
rule is meant to stop has already happened once in this exact problem space.

## Decision

We will replace the per-CLI UDS-dial mechanism ADR-0032's Consequences
section named with an aggregation model:

> **`trusty-console` gains a health probe that fans out over UDS to every
> trusty-\* service and aggregates the results. CLIs stop probing daemons
> point-to-point for status/health; they consume console's aggregated health
> instead of dialing each daemon's UDS socket themselves.**

Concretely, this decides *which component performs the UDS dial* for
status/health: `trusty-console`, singular, not each CLI independently. It
does **not**, by itself, decide how a CLI reaches console to obtain the
aggregated result, and it does not decide what a CLI should report when
console itself cannot be reached. Both are recorded below as open, not
resolved here.

## Open Questions

🔴 **Two questions are recorded as open. Neither is answered by this ADR.**

1. **How does a CLI reach console?** Per ADR-0032, console is the only HTTP
   surface, so an HTTP call from CLI to console for the aggregated health
   result is a legitimate design — but it reintroduces an HTTP dependency
   for what has otherwise become a UDS-only health path. Console-over-UDS
   (the CLI dials console's own UDS socket rather than console's HTTP port)
   is equally coherent and stays inside the UDS-only shape entirely. Both
   are live options; this ADR does not choose between them.

2. **What happens when console itself is down?** A fleet health probe that
   depends on the aggregator is blind in exactly the outage it exists to
   report. [#4925](https://github.com/bobmatnyc/trusty-tools/issues/4925)
   exists because `tctl` must say something useful when a service is
   stopped — and under this design, "a service" now includes the aggregator
   itself. The design needs a fallback path, or an explicit
   "console unreachable" outcome distinct from "service unhealthy," and
   this ADR does not specify one. This is the sharpest constraint on
   whatever implements this decision.

An ADR that pretended these were settled would be worse than one that names
them. This one names them.

## Consequences

**Easier / positive:**

- Collapses six independent per-CLI UDS-dial-plus-timeout-plus-failure-
  taxonomy implementations into one, in the same component ADR-0032 already
  made the singular HTTP surface — the same "one shared abstraction, not N
  migrations" shape ADR-0031 and ADR-0032 already required for the UDS
  transport itself, now applied to health specifically.
- Removes a duplication risk that has already materialized once in-tree
  (`trusty-installer`'s `probe_http.rs` alongside `trusty-common`'s
  `health_probe.rs` / `daemon_guard.rs` / `daemon_addr.rs`), rather than
  letting six CLIs recreate it a second time under the UDS migration.
- Gives #4925 one place to get "what does `tctl` say when a service is
  stopped" right, instead of six independently-implemented answers that can
  drift from each other.

**Harder / negative / trade-offs:**

- **Reintroduces, at the health layer, the exact tension ADR-0032's own
  topology is built on.** A single aggregator is a single point of failure
  for the signal that most needs to survive an outage. Open Question 2 names
  this directly and this ADR does not resolve it — an aggregator with no
  answer for "console is down" would be a regression from six independent
  dials, each of which fails only for its own daemon.
- **Open Question 1 leaves the CLI-to-console leg genuinely undecided.**
  Whichever answer is chosen, every CLI gains a new dependency on console
  specifically for health, where before each CLI's status command depended
  only on its own daemon.
- **This is a Medium, not Low, reversibility cost precisely because nothing
  was built yet.** The mechanism ADR-0032 described (per-CLI UDS dial) was
  never implemented before this ruling replaced it, so this amendment is
  cheap to make and would be cheap to reverse — worth stating so a future
  reader does not assume this undoes six crates' worth of shipped migration
  work.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior ADRs on 2026-08-08:

- **ADR-0032 (No trusty-\* service owns an HTTP daemon; UDS is the
  inter-service transport; console is the only HTTP surface):** **Amended.**
  This is ADR-0032's second amendment. ADR-0032's Decision is untouched and
  remains in force in full. This ADR replaces one mechanism named in its
  Consequences section (per-CLI UDS dial for status/health) with a
  console-side aggregator. ADR-0032's Status is updated to record both
  ADR-0034 and this ADR as amendments; its Context/Decision/Consequences are
  not edited (DOC-46 §4).

- **ADR-0034 (Webhook ingress: console relays over UDS to a supervised
  on-demand process, amends ADR-0032):** **Consistent — a sibling amendment,
  not an overlapping one.** Both this ADR and ADR-0034 amend ADR-0032, and
  both move a capability that ADR-0032 left as a per-service concern into
  `trusty-console`: ADR-0034 for webhook ingress, this ADR for health
  aggregation. Neither touches the other's mechanism. No conflict.

- **ADR-0031 (Transport by purpose: UDS inter-crate, HTTP external only):**
  **Consistent, and left open on one point by design.** Its classification
  test ("is the caller outside this host, or another crate on it?") still
  governs the console-to-service hop this ADR adds: console dialing each
  trusty-\* service's UDS socket is inter-crate traffic, squarely inside
  ADR-0031's UDS side. What ADR-0031's test does not settle — because this
  ADR does not settle it either — is the CLI-to-console leg, which is
  exactly Open Question 1 above.

- **ADR-0011 (`tctl` owns service lifecycle; console owns the single HTTP
  surface):** **Consistent.** Extending console's role to fleet health
  aggregation is not a new HTTP surface; it is the existing single surface
  gaining a new capability, which is the shape ADR-0011 already endorsed.

- **ADR-0019 (Unified IPC messaging on the event bus):** **Consistent — no
  interaction.** ADR-0019 governs durable, acknowledged cross-PM/cross-agent
  messaging. A health probe is a request/response poll, not bus messaging;
  noted because it is the other IPC-shaped decision in the corpus, not
  because the two overlap.

No conflicts with any other Accepted ADR. Summary: a second, independent
amendment to ADR-0032 (alongside ADR-0034), consistent with ADR-0031's
classification test on the console-to-service leg, and explicit that the
CLI-to-console leg and the console-down fallback are open questions this ADR
records but does not answer.
