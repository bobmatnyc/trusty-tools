# 0018. Loopback-only doctrine: `trusty-console` is the sole off-loopback HTTP surface

- **Status:** Accepted
- **Date:** 2026-07-19
- **Accepted:** 2026-07-19 (owner sign-off — Bob approved option (b) of the
  retire-direct-binds brief; tracked by epic #3328)
- **Scope:** Workspace-wide (every trusty-* daemon that serves HTTP:
  trusty-console, trusty-search, trusty-memory, trusty-analyze, trusty-review,
  trusty-agents, trusty-mpm)
- **Reversibility Cost:** Medium — rebinding a daemon's default address and
  wiring the shared origin guard is mechanical per-crate, but the doctrine
  itself is now cited by CI gates, docs, and multiple in-flight fix issues; a
  reversal would require re-litigating all of them.
- **Decision Drivers:** ADR-0011's unqualified "HTTP exactly once" claim vs.
  the six sibling daemons that each need a local HTTP port for CLI/status
  commands, MCP stdio-bridge proxying, and same-machine GUI clients; the
  unresolved "confirm during design" tension left open by #1222; the
  CSRF/origin-guard hardening already landed for console (#3268/#3269/#3280)
  and ported to the siblings (#3304); reqwest's lack of a Unix-domain-socket
  transport; GUI webviews' inability to `fetch()` a Unix socket.
- **Supersedes / Superseded by:** Amends **ADR-0011** (`0011` status updated
  to `Amended by 0018`; ADR-0011's Context/Decision/Consequences sections are
  left as originally accepted per the ADR immutability rule — DOC-46 §4 — this
  ADR is the record of the refinement, not an edit to that history).

## Context

**ADR-0011 (Accepted, 2026-06-15)** recorded the #1104/#1222 owner directive
as: "HTTP is implemented exactly once — in `trusty-console`. Local services
stay JSON-RPC-over-stdio (MCP); the console is the only HTTP surface." Issue
**#1222** (closed as the console front-door work landed) explicitly flagged
this as an open design tension and deferred it: "the 24/7 unattended
supervisor and durable tmux fleet need a persistent process, which is at odds
with 'HTTP only in console' … Confirm during design." That confirmation never
happened — the phrase "HTTP exactly once" shipped into ADR-0011 unqualified,
while the reference implementation's daemons kept, and grew, their own HTTP
listeners.

The **2026-07-19 architecture review** (`docs/research/architecture-review-2026-07-19.md`,
Appendix C) found the resulting gap between doctrine and reality: `trusty-search`,
`trusty-memory`, `trusty-analyze`, `trusty-review`, `trusty-agents`, and
`trusty-mpm` all bind their own HTTP listeners — every one of them serves CLI
commands (`<tool> status`, `<tool> port`), a native MCP stdio-bridge that
proxies to the daemon's own `/rpc`, and (for search/memory/analyze) an
embedded same-machine web UI. None of that is a violation waiting to be
retired: a CLI talking to `127.0.0.1:<port>` and a stdio-MCP bridge forwarding
JSON-RPC over HTTP to the same loopback port are not "a second HTTP surface"
in the sense ADR-0011's drivers (#1104/#1222) cared about — they never left
the machine. What ADR-0011's text failed to say is that this pattern was
always fine; it said "exactly once," which reads as a ban on any daemon-local
HTTP at all.

**Options considered to close the gap:**

- **(a) Full Unix-domain-socket (UDS) migration** — retire every daemon's TCP
  listener in favor of a UDS, eliminating "HTTP" from the sibling daemons
  entirely. **Rejected**: `reqwest` (the HTTP client used by trusty-console's
  reverse proxy and by three of the native MCP stdio bridges) has no UDS
  transport; each of those four call sites would need a bespoke
  `hyper`-over-UDS client. GUI webviews (mpm-gui, agents-ui, and the embedded
  search/memory/analyze SPAs) cannot `fetch()` a Unix socket at all — they
  require an `http://` origin. The migration cost and the webview blocker
  together made this a non-starter for the timeline.
- **(b) Loopback-only doctrine (chosen)** — keep sibling daemons on TCP,
  loopback-bound, and declare that pattern supported; reserve non-loopback
  binding (LAN, Tailscale, Funnel) exclusively for `trusty-console`. All
  external reachability — browsers off-machine, other machines, webhooks —
  goes through console's reverse proxy (`/api/{service}/*path`, #1849) and its
  Tailscale/Funnel bind modes (ADR-0017). This preserves ADR-0011's real
  intent (one place to reason about external exposure, auth, CORS, and
  DNS-rebind) without banning the loopback-local pattern every sibling daemon
  already depends on.

Bob approved option (b) on 2026-07-19 (epic #3328). This ADR records that
decision and resolves #1222's long-open "confirm during design" note.

## Decision

We will adopt the **loopback-only doctrine**:

> trusty-console is the **only HTTP surface reachable off-loopback** (LAN,
> Tailscale, Funnel). Sibling daemons (search, memory, analyze, review,
> agents, mpm) may each run their own **loopback-only** HTTP server for CLI
> use, MCP stdio-bridge proxying, and same-machine GUI clients — this is a
> supported pattern, not a violation. No daemon other than trusty-console may
> bind a non-loopback address; every daemon's write-mutating routes must run
> the shared `trusty_common::server` origin guard. External reachability
> (browsers, other machines, webhooks) is achieved exclusively via
> trusty-console's reverse proxy (`/api/{service}/*path`) and its
> Tailscale/Funnel bind modes.

This qualifies, but does not reverse, ADR-0011: the console remains the
**only** surface the outside world ever reaches; sibling daemons remain
**invisible** past the loopback interface. What changes is that ADR-0011's
"exactly once" language — which read as forbidding any daemon-local HTTP — is
replaced with an explicit two-tier model: loopback HTTP is universal and
permitted; off-loopback HTTP is console-exclusive.

## Consequences

**Easier / positive:**
- Resolves #1222's design tension explicitly instead of leaving it as an
  unconfirmed footnote — future readers of ADR-0011 no longer have to guess
  whether the reference implementation's daemon ports are a latent violation.
- No sibling daemon needs a UDS migration; CLI commands, stdio-MCP bridges,
  and embedded SPAs keep their existing `http://127.0.0.1:<port>` wiring
  unchanged.
- Gives the architecture-review's security findings (Appendix C) a single
  doctrine to check compliance against: `docs/reference/threat-model.md`
  (companion to this ADR) enumerates every daemon's bind address, guard
  status, and console-proxy status so drift is auditable going forward.
- Concentrates every off-loopback exposure decision (auth, CORS, DNS-rebind,
  TLS) in one crate (`trusty-console`), matching ADR-0011's and ADR-0017's
  original intent.

**Harder / negative / trade-offs:**
- The doctrine is currently **aspirational for two of seven daemons**:
  `trusty-agents` defaults to `0.0.0.0` with auth off by default (#3329, in
  flight) and is not yet in the console proxy allowlist; `trusty-mpm` still
  binds an optional `--tailscale` secondary listener that this doctrine makes
  redundant (#3330, in flight). Until those land, the doctrine is a stated
  target, not a fully-enforced invariant — `docs/reference/threat-model.md`
  marks both as "in progress" rather than claiming completion.
- A newly-discovered undeclared 7th HTTP surface (`trusty-agents`' per-project
  search-as-a-service daemon, #3335) falls outside every inventory this ADR
  assumed existed; it must be formally declared (loopback-bound, guarded) or
  retired before the doctrine can be called complete.
- ADR-0017's webhook ingress (trusty-analyze's `/webhooks/github`) predates
  this ADR and is not yet proxied through console — a known gap, tracked in
  `docs/reference/threat-model.md`'s Known Gaps section, for whoever lands
  ADR-0017.
- `trusty-mpm-gui` still talks directly to the trusty-mpm daemon rather than
  through the console gateway (#1849 Phase 2 migration tracked as #3333); it
  does not violate the doctrine (mpm-gui runs on the same machine as the
  daemon, i.e., still loopback-local) but it is the last GUI not yet
  gateway-first.

**Follow-up work (tracked under epic #3328):**
- #3329 — trusty-agents: default to loopback bind, require a token for any
  non-loopback opt-in.
- #3330 — trusty-mpm: remove the `--tailscale` secondary listener.
- #3333 — trusty-mpm-gui: migrate to console gateway-first resolution.
- #3335 — trusty-agents: formally declare (or retire) the per-project
  search-as-a-service daemon.
- Add `trusty-agents` to the console proxy allowlist once #3329 lands.

## Related Decisions

Vetted against prior ADRs on 2026-07-19:

- **ADR-0011 (`tctl` owns service lifecycle; `trusty-console` owns the single
  HTTP surface):** **Amended.** This ADR refines ADR-0011's "HTTP exactly
  once" language into the two-tier loopback/off-loopback model above; the
  core principle (console is the only externally-reachable surface) is
  preserved, not reversed. **Action:** ADR-0011's Status field is updated to
  `Amended by 0018`.
- **ADR-0017 (Shared webhook ingress via trusty-console + Tailscale Funnel,
  Proposed):** **Consistent / Extends.** ADR-0017 already assumed console-only
  off-loopback exposure ("trusty-mpm listens on localhost only… trusty-console
  handles all public HTTP/HTTPS binding") — this ADR generalizes that
  assumption from webhooks specifically to every daemon's HTTP surface. No
  conflict; ADR-0017's webhook-ingress gap (trusty-analyze's direct
  `/webhooks/github`, not yet retired or proxied) is carried forward as a
  known gap for whoever lands ADR-0017 (see `docs/reference/threat-model.md`).
- **ADR-0005 (shared HarnessEvent bus) / ADR-0004 (three harnesses on shared
  trusty-common):** **Consistent.** Neither addresses HTTP-surface topology;
  no interaction.

No conflicts with any other Accepted ADR. Summary: consistent with, and a
direct refinement of, ADR-0011; no silent contradictions.
