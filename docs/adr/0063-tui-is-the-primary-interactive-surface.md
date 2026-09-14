# 0063. TUI is trusty-code's primary interactive surface

- **Status:** Accepted
- **Date:** 2026-09-14
- **Scope:** crate `trusty-code`
- **Reversibility Cost:** Medium — the TUI (`tcode tui`, shared seam
  `crates/trusty-code-tui`, DOC-50) already exists as a shipped secondary
  entry point; flipping which entry point is primary changes doc framing and
  build priority, not the API surface or the thin-client boundary
- **Decision Drivers:** daemon-first independence (ADR-0058); a prototyping
  loop the owner can drive at a keyboard, with no tmux dependency, because
  `tcode` is a daemon service that keeps its own session state; reuse of the
  shared trusty-mpm metaharness — a PM that never codes directly and
  delegates, the same agent roster, and a trusty-code subset of core
  instructions written in our own words from published material; reduced
  pressure on the TCP `--http` transport the SPA/GUI would otherwise require
- **Supersedes / Superseded by:** —

## Context

DOC-39 (`docs/specs/trusty-code-harness-ui.md`) framed the SPA (web/Tauri) as
trusty-code's primary interactive platform and the TUI as a secondary entry
point (§1.2, §1.4 / `SPEC-TCUI-10~draft`). That framing predates two decisions
that now govern the same product: ADR-0058, which makes Trusty Code an
independent, product-owned harness with its own daemon and client transport,
and ADR-0059, which puts one canonical, host-neutral agent-behavior source
behind every trusty-* product, including `trusty-code`.

The owner ruled on 2026-09-14, verbatim: "confirmed on ADR flipping to TUI"
and "PM instruction text shared." The full direction behind that ruling:

- `trusty-code` reuses all of `trusty-mpm`'s metaharness work rather than
  reinventing orchestration.
- Orchestration is a PM that never codes directly and delegates to agents.
- `trusty-code` uses the same agent roster as `trusty-mpm`.
- No tmux integration — `tcode` is a daemon service that keeps its own
  session state, so it does not need a terminal multiplexer to survive
  client detach.
- Core instructions are a `trusty-code` subset, written in the project's own
  words from published material, not copied verbatim.
- The product leads with a TUI modelled on Claude Code, because that is the
  surface the owner can prototype directly at a keyboard. Ordering: TUI
  first.

The TUI is not new work this ADR authorizes. `crates/trusty-code-tui` and
`tcode tui` already ship (DOC-50, `docs/specs/DOC-50-tcode-tui-claude-code-clone.md`).
What changes is which entry point DOC-39 names as primary, and therefore
which surface trusty-code's build and documentation prioritize next.

The thin-client axiom is unaffected by this choice. DOC-39 §2.1
(`SPEC-TCUI-09~draft`) states it as an owner directive binding over every
other section of that document: the UI communicates with the daemon, the
daemon provides all functionality, and no UI target — TUI or SPA — carries
business logic, filesystem access, or process/git access of its own. DOC-50
§2.1 (`SPEC-TTUI-09~draft`) already restates this constraint as binding on
the TUI specifically. Flipping which surface ships first does not touch this
constraint; both surfaces remain daemon-driven thin clients under it.

## Decision

We will make the interactive TUI (`tcode tui`, shared seam
`crates/trusty-code-tui`, DOC-50) trusty-code's **primary** interactive
surface. The SPA/Tauri platform described in DOC-39 §1.2 is **deferred, not
dropped** — it remains the eventual second UI target, but the TUI ships and
is documented first.

The thin-client axiom (DOC-39 §2.1 / `SPEC-TCUI-09~draft`, restated for the
TUI at DOC-50 §2.1 / `SPEC-TTUI-09~draft`) stays binding without change:
every fact the TUI shows arrives from a daemon call or event; no UI target
reads the filesystem, spawns a process, or shells out to `git` directly.

`trusty-code` reuses the trusty-mpm metaharness rather than building its own
orchestration layer: a PM that never codes directly, the same agent roster
trusty-mpm uses, and a trusty-code subset of core instructions written in
this project's own words. The TUI needs no tmux integration, because `tcode`
is a daemon service that keeps session state itself — a client can detach
and reattach without a terminal multiplexer holding the session alive.

## Consequences

**Easier:**

- The owner gets a prototyping loop at a keyboard now, instead of waiting on
  SPA/Tauri packaging work.
- `crates/trusty-code-tui` and DOC-50 move from "secondary entry point" to
  the documented main line of trusty-code's interactive build, which matches
  where implementation effort already sits.
- Dropping tmux as a dependency removes a process-supervision integration
  the daemon-owned session model (ADR-0058) does not need: `tcode` already
  keeps session state durably, so nothing external has to keep a pane alive.
- GUI/SPA-driven pressure on the TCP `--http` transport is reduced for now,
  since no SPA client ships in this phase. This strengthens the ADR-0058
  UDS-first transport direction: fewer near-term callers need the loopback
  TCP listener ADR-0058 already tracks as an interim surface.
- Reusing the trusty-mpm metaharness (PM, agent roster, instruction style)
  means trusty-code's orchestration layer inherits a maintained, tested base
  instead of duplicating it.

**Harder / costs:**

- DOC-39's product framing (§1.2) and secondary-entry-point section (§1.4)
  needed amendment so the document matches the ordering decision; a reader
  following only DOC-39's original text would have shipped the wrong surface
  first. This ADR is the record of why the correction happened.
- The SPA/Tauri platform stays specified but unscheduled. Nothing in DOC-39
  names a resumption trigger; a future ADR or DOC-39 revision must set one
  before SPA work restarts, or the platform risks silent abandonment instead
  of a deliberate deferral.
- Two live specs (DOC-39, DOC-50) now describe entry-point primacy from
  different directions — DOC-39 defers to this ADR for the ordering call,
  DOC-50 gains a cross-reference (§9) pointing back here. Both must be kept
  in sync on future entry-point changes, or the specs will drift apart on
  which surface is primary.

**Unaffected:**

- DOC-73 dashboard and any GUI work already committed keep the JSON-RPC
  surface as their contract. Nothing about this ADR touches the daemon API;
  the TUI and any future SPA are two clients of the same surface (DOC-39
  §2.1, C-3: no capability divergence between targets).
- The thin-client axiom, C-1 through C-4 (DOC-39 §2.1), is unchanged and
  remains binding on every current and future trusty-code UI target.

## Related Decisions

Vetted against prior ADRs on 2026-09-14:

- **ADR-0004 (Three harnesses share event-driven common infrastructure):**
  Consistent. Amended by ADR-0058 for the `trusty-code` boundary; this ADR
  picks an entry-point ordering within that boundary and does not touch the
  shared-infrastructure decision.
- **ADR-0005 (Shared harness event bus):** Consistent. Its P3/P4 items
  define the SSE/WebSocket wire shape for browser/Tauri subscribers; that
  work stays specified and unstarted while the SPA is deferred, and nothing
  here removes it from the bus's roadmap.
- **ADR-0031 (Transport by caller purpose — UDS inter-crate, HTTP
  external):** Extends. ADR-0031 already treats embedded SPAs and Tauri
  webviews as the reason a daemon keeps an HTTP path at all. Deferring the
  SPA removes that reason for `trusty-code` in the near term, so the TCP
  `--http` listener ADR-0058 tracks as interim carries less live traffic —
  this ADR extends ADR-0031's transport-pressure analysis rather than
  changing its rule.
- **ADR-0032 (Console is the only HTTP surface):** Consistent. `trusty-code`
  gains no new HTTP surface from this decision; a TUI talks to the daemon
  over the same client transport ADR-0058 already defines, not a new port.
- **ADR-0058 (Trusty Code is an independent, product-owned harness):**
  Extends. ADR-0058 established the daemon, its client transport, and the
  UDS-first direction with loopback TCP as a tracked interim. This ADR does
  not reopen that transport decision; it picks which client ships first
  against it and, by deferring the SPA, reduces near-term pressure toward
  the TCP interim ADR-0058 already flagged as provisional.
- **ADR-0059 (Canonical agent behavior, generated host adapters):**
  Consistent. `trusty-code` reusing the trusty-mpm metaharness — the same PM
  model, the same agent roster, an instruction subset in the project's own
  words — is the ADR-0059 model applied to a second product; it introduces
  no second authoring source.
- **ADR-0060 (One MCP config authority in trusty-mcp):** Consistent.
  Unaffected — this ADR does not touch MCP server configuration or its
  authority, only which trusty-code UI ships first.

No prior decision contradicts this choice. Summary: Consistent with, and in
two cases (ADR-0031, ADR-0058) reinforcing, the existing decision set; no
unresolved conflicts.
