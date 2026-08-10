# 0033. `trusty-mcp` consolidates native MCP services into one adapter crate — amends ADR-0014

- **Status:** Superseded by [0040](0040-trusty-mcp-services-absorbs-trusty-gworkspace.md) — 0040 records this ADR as categorically mistaken about `trusty-channels` (framework-consumed, not MCP) and unfounded on `trusty-kb` (its obsolescence premise doesn't hold); only the instinct that `trusty-gworkspace` shouldn't stay a standalone crate survives, and not in its specifics
- **Date:** 2026-08-07
- **Scope:** Workspace-wide (`trusty-gworkspace`, `trusty-channels`, `trusty-kb`,
  plus every call site and trust-config table that names them)
- **Reversibility Cost:** Medium — a crate rename/merge, not a protocol or
  transport change. Reversible by re-splitting modules back into standalone
  crates, at the cost of re-publishing three crate names once any of them has
  shipped a live version — the exact window this ADR spends before it closes.
- **Decision Drivers:** owner directive on crate packaging (2026-07-27,
  restated 2026-08-07); epic
  [#5066](https://github.com/bobmatnyc/trusty-tools/issues/5066); free-rename
  window closing at first publish
- **Supersedes / Superseded by:** **Amends ADR-0014** (does not supersede it —
  see Related Decisions). Supersedes the standalone-publish intent of
  [#4048](https://github.com/bobmatnyc/trusty-tools/issues/4048) and
  [#4047](https://github.com/bobmatnyc/trusty-tools/issues/4047) — see
  Consequences.

## Context

### What exists today

`crates/trusty-mcp/` **does not exist and never has** — absent from
`origin/main` and from all git history. Three separate crates ship native MCP
servers that would consolidate into it:

| Crate | Binary(ies) | Approx. production SLOC | Publish state |
|---|---|---|---|
| `trusty-gworkspace` | `trusty-gworkspace-mcp` | ~15.2k | Never published a live version |
| `trusty-channels` | `slack-mcp`, `telegram-mcp` | ~6.4k | Never published a live version |
| `trusty-kb` | `trusty-kb` | ~4.0k | Never published a live version (its own CHANGELOG says so explicitly: "This crate has not cut a release yet") |

~25.6k SLOC total, before counting the unscoped call-site and trust-config
rewiring this decision also requires.

Verified directly against the crates' `Cargo.toml` files on this branch: none
of the three currently sets a `publish` field (all default-publishable).
Epic #5066 describes `trusty-gworkspace` and `trusty-channels` as carrying
`publish = false` — that no longer matches what's on disk, but the underlying
fact the epic cites is still true and is the one this ADR relies on: **none of
the three has ever shipped a live crates.io version.** The free-rename window
is open only until the first one does.

### Direct precedent: `trusty-common` already does this

`trusty-common`'s `Cargo.toml` documents two prior absorptions:

- `tickets-mcp` — "Unified ticketing MCP server (absorbed from
  trusty-tickets)," gated behind `required-features = ["tickets"]`.
- The `mcp` module itself — "formerly the `trusty-mcp-core` crate" (commit
  `a90ba071`, cited in ADR-0014's Context).

Both are real precedent for folding a standalone MCP crate into a shared one
without ceremony. Both differ from this decision in one respect worth naming:
they were **library absorptions into `trusty-common`**, an existing crate that
already owned the shared framework. This decision proposes a **new adapter
*binary* crate** (`trusty-mcp`) purpose-built to hold what were three separate
binaries, not a fold into an existing library. The precedent supports the
*pattern* (native MCP surfaces consolidate rather than proliferate); it
doesn't by itself cover the binary-collocation question this ADR leaves open
below.

### What this is not: a return to the MCP-A gateway

[DOC-27](../specs/SPEC-MCPSVC-01-trusty-mcp-service.md) (`SPEC-MCPSVC-01`
through `-07`) proposed a unified `trusty-mcp-service` crate in 2026-06-23 and
was explicitly **not adopted** — its own header says so, citing ADR-0014.
DOC-27's design was substantially heavier than what's being authorized here:
a new **MCP-A protocol** collapsing every tool into 7 generic primitives
(`discover`/`schema`/`query`/`follow_up`/`context`/`explain`/`action`), a new
`AuthProvider` trait with OAuth2 PKCE and a two-tier credential store, an
in-process answer/action store with TTL eviction, and an always-on HTTP daemon
+ stdio proxy pair — none of which exists today, all of which DOC-27 asked to
build from scratch.

This decision does not resurrect that design. `trusty-mcp` keeps each
service's existing native stdio MCP tool surface exactly as it is today — the
same tool names, the same per-tool JSON schemas, the same `tools/list` +
`match` dispatch convention ADR-0014 already established. What moves is
**packaging**: three crates and their `Cargo.toml`s become one crate with
three (or fewer — see Open Questions) library modules and adapter binaries.
No new protocol, no new auth abstraction, no new daemon.

What actually changed between DOC-27 (2026-06) and now is not a new technical
argument — it's that the owner changed his mind about crate granularity, and
said so twice in the same hour on 2026-08-07 while filing and amending
#5066. The Context above documents the free-rename-window argument because
it's real and it's the reason to act now rather than later, not because it
was absent from the original ADR-0014 discussion.

### Scope: what moves, what stays, what blocks

| | Status |
|---|---|
| `trusty-gworkspace` (Gmail/Calendar/Drive/Docs/Sheets/Slides/Tasks) | **Moves** into `trusty-mcp` as a library module |
| `trusty-channels` (Slack, Telegram) | **Moves** into `trusty-mcp` as a library module |
| `trusty-kb` (personal-KB MCP server) | **Moves** into `trusty-mcp` as a library module |
| `trusty-memory` | **Stays out.** `serve --stdio` is already a pure proxy to the daemon (#1078); `trusty-mcp` dials it, it does not absorb it |
| `trusty-search` | **Stays out.** Same reason — already a pure proxy |
| `trusty-mpm` (`tm mcp`) | **Stays out.** Same reason — already a pure proxy |
| `trusty-review` | **Blocked from joining** until [#5064](https://github.com/bobmatnyc/trusty-tools/issues/5064) lands — its stdio path still opens `dedup.redb` directly instead of proxying to the daemon, which would collide with a co-resident `serve` process the way #3992 already documented for trusty-memory pre-#1078 |
| `tc-services` and `trusty-agents` (via its bridge modules — the path other Trusty personas including cto-assistant reach `trusty-gworkspace` through) | Call `trusty_gworkspace` as a **library**, not through MCP. Consolidation requires a stable public API surface inside `trusty-mcp`'s `[lib]` target plus an import-path migration in these callers — confirmed present as `trusty-gworkspace = { path = "../trusty-gworkspace", … }` in both `crates/tc-services/Cargo.toml` and `crates/trusty-agents/Cargo.toml` |
| Transport (HTTP vs. UDS vs. stdio) | **Untouched, set by ADR-0032.** `trusty-mcp` does not build its own transport ahead of ADR-0032's UDS-for-inter-service / console-owns-HTTP model — see Related Decisions |

Implementation, including the sub-issue breakdown epic #5066 deliberately
withholds until work starts, is **1.3.6+ — not 1.3.5.** This ADR authorizes
and scopes the consolidation; it does not schedule it.

## Decision

We will consolidate `trusty-gworkspace`, `trusty-channels`, and `trusty-kb`
into a single new crate, `trusty-mcp`, as a protocol adapter and peer of
`trusty-console`: `trusty-console` speaks HTTP outward for browsers,
`trusty-mcp` speaks MCP outward for MCP clients, and both dial the same
backing daemons rather than owning their own storage or business logic.

Concretely:

- The three source crates' modules move into `trusty-mcp` as library modules;
  their existing binaries (`trusty-gworkspace-mcp`, `slack-mcp`,
  `telegram-mcp`, `trusty-kb`) become adapter binaries inside the new crate
  (exact binary-collocation shape is an Open Question below).
- `trusty-gworkspace`, `trusty-channels`, and `trusty-kb` as standalone crate
  names are retired. Because none has ever published a live crates.io
  version, this retirement costs nothing in the registry today; it would cost
  a deprecation/yank cycle per crate if deferred past a first publish.
- `trusty-mcp` additionally dials `trusty-search`, `trusty-memory`, and
  `trusty-mpm` for the tool surfaces they already expose as pure stdio
  proxies (#1078) — a re-point of existing wiring, not new absorption.
- `trusty-review` joins once, and only once, #5064 converts its stdio path
  into a proxy.
- `trusty-mcp` uses whatever inter-service transport ADR-0032 specifies (UDS
  for same-host, dialing the target daemons) — this ADR adds no transport
  decision of its own.

## Consequences

**Easier / positive:**

- One crate, one `cargo install`, one Developer-ID-signing target for every
  bundled native MCP surface instead of three (soon more, as
  `trusty-review` joins) — directly reduces the packaging surface ADR-0014's
  "single install story" argument was about.
- Closes the free-rename window while it's free. Waiting past a first
  `cargo publish` of any of the three crates would turn this from a rename
  into a yank-and-redirect.
- Supersedes the standalone-publish intent of #4048 (`trusty-gworkspace`) and
  #4047 (`trusty-channels`) — those issues asked "should this publish under
  its own name," and this decision answers "no, it publishes as part of
  `trusty-mcp` instead." Both issues should be closed or re-scoped once
  `trusty-mcp` exists, referencing this ADR.
- Matches the precedent `trusty-common` already set twice (`tickets-mcp`,
  `trusty-mcp-core`) — reviewers and future maintainers have a known shape to
  recognize this against.

**Harder / negative (honest trade-offs):**

- **The install/signing story ADR-0014 optimized for is not obviously
  preserved.** ADR-0014's stated reason for one-crate-per-service was that
  "each surface keeps its own `cargo install` / Developer-ID-signing story."
  Collapsing three services into one crate means one crate-level
  version bump now ships all three services together — a Slack-only fix
  forces a version bump (and re-signing, re-installing) for Google Workspace
  and the KB server too. This ADR does not resolve that tension; see Open
  Questions.
- **Import-path migration is unscoped work, not a rename script.**
  `tc-services` and `trusty-agents` (its gworkspace bridge modules, the path
  cto-assistant and other personas reach `trusty_gworkspace` through) call it
  as a library today. `trusty-mcp` must expose a stable public API for
  gworkspace's client + service handlers, and both callers need their `use
  trusty_gworkspace::…` imports migrated to whatever `trusty-mcp` exposes
  them as.
- **Credential and config-shape heterogeneity moves into one crate.**
  gworkspace's two-tier OAuth token file, Slack/Telegram's static bot tokens,
  and the KB server's filesystem-tree config today live in three crates with
  independent assumptions. Folding them into one `[lib]` target doesn't
  resolve how they coexist — see Open Questions.
- **#5066 explicitly withholds sub-issue breakdown until work starts** — this
  ADR authorizes the shape, not a work plan. The engineer who picks this up
  in 1.3.6 files the sub-issues then, informed by whatever has changed by
  then (e.g. #5064's landing, ADR-0032's webhook-ingress open question).

## Open Questions

- **Do the absorbed surfaces keep separate binaries, or collapse to one?**
  DOC-27's abandoned design used two binaries total (`trusty-mcp-serviced`
  daemon + `serve --stdio` proxy) fronting all domains. This ADR's simpler
  adapter-crate shape could go either way: keep `trusty-gworkspace-mcp`,
  `slack-mcp`, `telegram-mcp`, `trusty-kb` as four separate `[[bin]]` targets
  inside one crate (minimal disruption to existing `.mcp.json` wiring), or
  collapse to a single `trusty-mcp` binary with a subcommand or
  domain-selection flag per service (closer to the "protocol adapter, peer
  of trusty-console" framing in the Decision). Not decided here.
- **How do per-service credentials and differing config shapes coexist in
  one crate?** gworkspace's OAuth two-tier token file, Slack/Telegram's
  static bot tokens, and the KB server's filesystem-tree config need either a
  shared credential abstraction or a documented "each module keeps its own
  scheme, `trusty-mcp` doesn't unify it" decision. Not decided here.
- **What happens to the install/signing story ADR-0014 optimized for?** This
  is the strongest argument against consolidating, and it is not resolved by
  this ADR. ADR-0014 chose one-crate-per-service specifically so a change to
  one service's MCP surface doesn't force a version bump, re-sign, and
  re-install of every other service sharing its crate. Consolidating
  reintroduces exactly that coupling. Candidate mitigations — feature-gated
  binaries so an unaffected service's binary is bit-identical across a bump,
  or accepting the coupling as a fair price for one install target — are not
  evaluated here and should be settled before or during 1.3.6
  implementation, not discovered mid-PR.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-08-07:

- **ADR-0014 (Ship full native MCP support, Accepted 2026-07-14): Amended by
  this ADR.** ADR-0014's Decision said new MCP-exposed surfaces are "built
  in-workspace, in Rust, on the shared `trusty-common` `mcp` feature" — that
  survives untouched; this ADR does not reopen native-vs-third-party at all.
  What ADR-0014 did not decide, and what this ADR changes, is packaging
  granularity: ADR-0014's Context describes trusty-gworkspace and (by
  implication, per its "and more" framing and the crates it names as already
  consuming the shared `mcp` feature) the rest of the native MCP surface as
  living in their own crates; this ADR says three of those crates fold into
  one. ADR-0014's own Consequences already flagged "hand-written match
  dispatch per server… scales linearly with tool count per server" as a
  known cost — this ADR doesn't fix that either; it only changes which
  `Cargo.toml` the dispatch tables live under. **Boundary, stated precisely:**
  the native-MCP-in-Rust commitment survives in full; only the
  one-crate-per-service packaging choice is superseded by this ADR's
  one-crate-for-these-three-services packaging. Nothing here casts doubt on
  building MCP servers natively in Rust — that question is not reopened.
- **DOC-27 / SPEC-MCPSVC-01 (unified MCP-A gateway, Superseded 2026-06-23):**
  Consistent, not resurrected. DOC-27 was already superseded by ADR-0014
  before this ADR existed; this ADR does not revive it. See "What this is
  not" in Context for the specific differences (no MCP-A protocol, no new
  auth trait, no daemon).
- **ADR-0031 (Transport by purpose, Proposed 2026-08-06) / ADR-0032 (No
  service owns HTTP, Accepted 2026-08-07):** Consistent, orthogonal axis.
  Both govern *transport* — inter-service traffic over UDS, `trusty-console`
  as the sole HTTP surface. This ADR governs *crate structure* — how many
  crates the native MCP servers live in. `trusty-mcp` is a transport
  *client* under ADR-0032's model (it dials search/memory/mpm/review the way
  any other caller would); it does not define, extend, or contradict how
  those dials happen. The two decisions compose: ADR-0032 says how
  `trusty-mcp` reaches its backing daemons, this ADR says what crate
  `trusty-mcp`'s own code lives in.
- **#4048 / #4047 (standalone crates.io publication for `trusty-gworkspace` /
  `trusty-channels`):** Superseded in intent by this ADR — see Consequences.
  Not formal ADRs, so no status field to flip; recorded here so whoever
  triages them next finds the answer.

No other Accepted ADR addresses MCP crate packaging or the specific crates
this ADR names. No conflicts requiring resolution beyond the ADR-0014
amendment boundary stated above.

## References

- Epic [#5066](https://github.com/bobmatnyc/trusty-tools/issues/5066) —
  target shape for `trusty-mcp`, filed and amended twice 2026-08-07
- Related: [#3828](https://github.com/bobmatnyc/trusty-tools/issues/3828) —
  earlier "unified CLI multiplexer" framing of the same consolidation idea
- [#5064](https://github.com/bobmatnyc/trusty-tools/issues/5064) — hard
  prerequisite for `trusty-review` joining `trusty-mcp`
- [#4048](https://github.com/bobmatnyc/trusty-tools/issues/4048),
  [#4047](https://github.com/bobmatnyc/trusty-tools/issues/4047) — standalone
  publish requests this ADR supersedes
- `crates/trusty-common/Cargo.toml` — `tickets-mcp` and `mcp`
  (ex-`trusty-mcp-core`) absorption precedent
- `crates/tc-services/src/gworkspace.rs`,
  `crates/tc-services/Cargo.toml:28`, `crates/trusty-agents/Cargo.toml:78` —
  confirmed direct library dependents of `trusty_gworkspace`
- [docs/adr/0014-native-mcp-support.md](0014-native-mcp-support.md) — the
  amended ADR
- [docs/specs/SPEC-MCPSVC-01-trusty-mcp-service.md](../specs/SPEC-MCPSVC-01-trusty-mcp-service.md)
  (DOC-27) — the superseded unified-gateway design, not resurrected here
- [docs/adr/0032-no-service-owns-http-console-is-the-only-http-surface.md](0032-no-service-owns-http-console-is-the-only-http-surface.md) —
  governs `trusty-mcp`'s transport to its backing daemons
