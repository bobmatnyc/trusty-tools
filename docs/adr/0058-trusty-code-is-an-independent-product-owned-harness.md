# 0058. Trusty Code is an independent, product-owned coding harness

- **Status:** Accepted
- **Date:** 2026-09-03
- **Scope:** crate `trusty-code` — runtime, daemon, configuration roots, private
  state, delegated coding tasks, client transport, and structured integration
  with `trusty-mpm`
- **Reversibility Cost:** High — the state root, client transport, task
  ownership, and integration boundary shape every Trusty Code client and every
  migration path off `.claude/`
- **Decision Drivers:** independent releaseability; the owner ruling that Trusty
  Code's native root is `.trusty-code/`; durable coding work that survives
  client detach; shared MCP and channel contracts that do not make `trusty-mpm`
  the runtime owner; [#5433](https://github.com/bobmatnyc/trusty-tools/issues/5433)
  finding that accepted records disagree on all four boundaries
- **Supersedes / Superseded by:** Amends ADR-0004, ADR-0024, and ADR-0042 for
  the `trusty-code` boundary; supersedes nothing

## Context

Trusty Code has grown from a scaffold into a daemon-backed coding harness with
JSON-RPC clients, session events, workstreams, agents, skills, plugins, and an
HTTP/SSE client API. The workspace decisions that describe it were written
before that runtime existed, and #5433's audit found four unresolved
ambiguities:

1. whether Code is an independent product or a `trusty-mpm` execution detail;
2. whether its native files belong under `.claude/`, `.open-mpm/`, or a
   product-owned root;
3. whether delegated coding agents are ephemeral in-process leaves under
   ADR-0024 or durable daemon-owned tasks; and
4. whether its local client API is compatible with ADR-0032, which makes
   `trusty-console` the only HTTP surface.

Those ambiguities block a clean independent release and let one harness
overwrite another harness's state.

The fourth ambiguity has since been settled by the owner rather than left open.
The 2026-08-19 ruling recorded on
[#6637](https://github.com/bobmatnyc/trusty-tools/issues/6637) is verbatim:

> Unix domain socket (UDS), not HTTP+auth. Implementation should replace the
> loopback TCP listener with a UDS endpoint (filesystem permissions as the auth
> boundary) rather than adding token auth to HTTP.

This ADR records that ruling as Code's target transport rather than carving an
HTTP exception out of ADR-0032.

## Decision

1. **Trusty Code is an independent product.** Its daemon owns coding sessions,
   workstreams, delegated coding tasks, event history, and resumable execution.
   `trusty-mpm` may orchestrate Code through a versioned structured API; it does
   not own Code's runtime and does not scrape terminal panes to infer Code
   state.
2. **Trusty Code separates project configuration from private runtime state.**
   `<project>/.trusty-code/` is the native root for declarative project
   configuration, generated adapters, manifests, and provenance.
   `~/.trusty-code/` is the private root for mutable runtime state: credentials,
   transcripts, logs, channel cursors, caches, process/task/session metadata,
   and resolved executable MCP lifecycle data. Host directories such as
   `.claude/` and `.codex/` are import and export targets, never Code's source
   of truth.
3. **Delegated Code agents are daemon-owned durable tasks.** Client attachment
   is optional and repeatable; detaching a CLI or TUI does not cancel a task.
   Cancellation, authorization, and credential grants stay explicit at each
   delegation boundary.
4. **Trusty Code's client transport converges on UDS under ADR-0032.** Local
   JSON-RPC over stdio remains a supported client surface. The loopback TCP
   HTTP/SSE listener is interim compatibility, not a permitted product HTTP
   surface: it is bearer-authenticated today
   ([#6491](https://github.com/bobmatnyc/trusty-tools/pull/6491), shipped in
   `trusty-code` 0.5.0) and is retired by #6637. Code never binds off-loopback
   and never becomes the workspace's public ingress; `trusty-console` remains
   the only externally reachable HTTP surface.
5. **MCP and channel interoperability is contract sharing, not state sharing.**
   Code imports versioned MCP server definitions and channel envelope schemas,
   records non-secret project references and provenance under
   `<project>/.trusty-code/`, resolves trusted executable definitions and
   mutable lifecycle state under `~/.trusty-code/`, and owns process lifecycle
   for the servers and tasks it starts. Imported executable definitions require
   explicit provenance and trust validation. Code does not mutate MPM's managed
   configuration.
6. **Cross-product integration is adapter-based and versioned.** MPM receives
   structured task identity, lifecycle, status, event, cancellation, and result
   contracts. Compatibility failures are explicit; no path-based aliasing or
   best-effort parsing substitutes for a negotiated contract.

## Consequences

- Trusty Code can be installed, operated, upgraded, and released without a
  running MPM service.
- Existing `.claude/` agent, skill, and plugin readers become compatibility
  adapters with an explicit migration path to `.trusty-code/`.
- Project configuration can be reviewed and shared without committing
  transcripts, secrets, logs, executable resolution state, or session data.
  Private state requires restrictive permissions and symlink-safe writes.
- In-memory session state is insufficient for release readiness. `SessionRegistry`
  is in-memory only today, so daemon-owned task identity and recovery are
  release work, not a shipped property.
- The GUI blocks the transport conversion. `trusty-code-gui`'s webview dials
  `http://127.0.0.1:7882` from TypeScript, and neither `fetch` nor `EventSource`
  can open a UDS, so #6637 needs a design round before it can be scheduled. The
  interim bearer auth stays in force until it lands and does not substitute for
  it.
- MPM integration work can proceed independently against a stable API contract;
  its implementation stays outside the `trusty-code` crate.

## Related Decisions

Vetted against the ADR corpus on 2026-09-03:

- **ADR-0004 (Three event-driven harnesses):** Amends — preserves the peer
  harness model and adds Trusty Code's ownership boundary now that its daemon
  exists.
- **ADR-0005 and ADR-0019 (Shared event bus and unified IPC):** Extends — Code
  adopts the shared envelope and acknowledged messaging contracts through a
  product-owned adapter. Its `SessionEventEnvelope` stays a local stream and is
  not that bus.
- **ADR-0014 and ADR-0040 (Native MCP support and packaging):** Consistent —
  Code consumes MCP contracts without changing which crates implement native
  services.
- **ADR-0024 (In-process leaf sub-agents):** Amends — that rule stays in force
  inside `trusty-agents`; a delegated *Trusty Code coding task* is a
  cross-product daemon task, not an Agents L1 leaf.
- **ADR-0026 (Credential grants stop at delegation):** Consistent — durable task
  identity does not imply inherited credentials.
- **ADR-0031 (Transport by caller purpose):** Consistent — stdio is a local
  product-client transport and UDS is the same-host transport this record
  converges on.
- **ADR-0032 (Console is the only HTTP surface):** Consistent, not amended — the
  2026-08-19 owner ruling on #6637 applies ADR-0032's UDS decision to Trusty
  Code rather than carving an exception. The surviving loopback TCP listener is
  a tracked deviation, not a permission.
- **ADR-0042 (Static persistent MCP configuration):** Amends — user-level
  declarations stay static; Code resolves imported declarations into its own
  private state and never injects them into a workspace or into MPM's
  configuration.
- **ADR-0044 (Main-checkout write boundary and agent worktree ownership):**
  Consistent — Code owns the worktrees and task state for agents it launches.
- **ADR-0059 (Canonical agent behavior):** Extends — defines how shared authored
  behavior enters this product-owned boundary.

The amendments above resolve every conflict the #5433 audit identified. No other
Accepted ADR assigns Trusty Code runtime or native state ownership to another
product.
