# 0017. Shared webhook ingress via trusty-console + Tailscale Funnel

- **Status:** Proposed
- **Date:** 2026-07-19
- **Scope:** Workspace-wide (trusty-mpm, trusty-console, integrations)
- **Supersedes / Superseded by:** —
- **Related Specs:** DOC-47 (external event ingestion); ADR-0005 (event bus); ADR-0011 (console as single HTTP surface)

## Context

External event ingestion (DOC-47) requires a public HTTPS endpoint to receive webhooks from GitHub, JIRA, Slack, Telegram, Google, and other sources. We need to decide:

1. **Where to mount it.** In trusty-mpm's daemon, or elsewhere?
2. **How to expose it publicly.** Cloud relay, inbound firewall rules, Tailscale Funnel?
3. **What architectural pattern avoids per-source endpoint sprawl.** One endpoint per source type, or one shared ingress?

### Existing Conventions

- **ADR-0011** established trusty-console as the single HTTP surface for the entire platform. All internal and external HTTP traffic flows through console's reverse proxy (`ANY /api/{service}/{*path}`).
- **trusty-console/bind.rs** already supports `--tailscale` (tailnet-only binding) via Tailscale's local API socket.
- **trusty-mpm/daemon/api.rs** has a precedent `POST /hooks` handler for Claude Code hooks (issue #1970).
- **Project context** (CLAUDE.md) specifies zero cloud infrastructure as the default; documented later upgrade paths (phase 2) are acceptable.

### Design Alternatives

**A. Per-source endpoints (rejected)**
- Each adapter (GitHub, JIRA, Slack, etc.) defines its own `/api/{service}/webhook` route
- Consequences:
  - Multiple public endpoints to expose, each with separate DNS/binding decisions
  - Verification logic (signature checking) scattered across multiple handlers
  - Harder to audit for security regressions (no single code path)
  - Violates "single HTTP surface" principle (ADR-0011)
- **Verdict: Rejected.** Endpoint sprawl contradicts established conventions.

**B. Shared ingress in trusty-mpm, directly internet-exposed (rejected)**
- Mount `/api/webhooks/{source}` in trusty-mpm daemon
- Use `--listen 0.0.0.0:443` (inbound firewall rules required)
- Consequences:
  - trusty-mpm becomes a direct public service, breaking containment (it's a control plane, not a public API server)
  - Requires inbound firewall management per user (especially problematic on macOS)
  - TLS termination in trusty-mpm instead of a dedicated reverse proxy
  - Higher blast radius if trusty-mpm has a network security bug
- **Verdict: Rejected.** Control-plane containment is architecturally important.

**C. Shared ingress via trusty-console (Tailscale Funnel) — CHOSEN**
- Mount webhook handlers in trusty-mpm's daemon API
- Route through trusty-console's existing reverse proxy (`ANY /api/{service}/{*path}`)
- Expose publicly via Tailscale Funnel (no inbound firewall rules needed)
- Consequences (see Consequences section below)
- **Verdict: Chosen.**

**D. Cloud relay (deferred)**
- A thin cloud service (receive + queue + forward)
- Eliminates the need for any local public binding
- Documented as a phase 2 upgrade when on-premises deployment becomes common
- **Verdict: Deferred to phase 2.** Not in scope of this ADR/spec.

## Decision

**We will implement shared webhook ingress via trusty-console's reverse proxy, using Tailscale Funnel as the public-exposure mechanism.**

Specifically:

1. **Extend trusty-console's BindMode** with a new `Funnel { tailnet_name: Option<String> }` variant
   - Default behavior: when started without `--listen 127.0.0.1`, enable Funnel
   - Funnel uses Tailscale's regional relay servers; no local inbound firewall needed
   - Provides HTTPS with Tailscale-issued TLS certs (no cert management burden)

2. **Route all webhooks through one `/api/webhooks/{source}` endpoint**
   - Mounted in trusty-mpm's daemon API (`crates/trusty-mpm/src/daemon/api.rs`)
   - Reverse-proxied by trusty-console's existing handler (`ANY /api/{service}/{*path}`)
   - Signature verification happens at the webhook handler before any payload processing

3. **Preserve trusty-mpm as a control plane, not a public API server**
   - trusty-mpm listens on localhost only (bound to 127.0.0.1)
   - trusty-console handles all public HTTP/HTTPS binding and TLS
   - Maintains clean separation of concerns (ADR-0011)

4. **Document the phase 2 cloud relay as a documented upgrade path**
   - If on-premises deployments become common and local Funnel binding is insufficient, the cloud relay upgrade is a documented later choice, not a blocking decision now

## Consequences

### Easier

- **One verification seam.** All signature checks happen in one place (`ingest_webhook` in trusty-mpm). Audit and security review are simpler.
- **No firewall management.** Tailscale Funnel is automatically public; no user has to open inbound ports on their machine. This is especially valuable on macOS, where firewall rules are persistent and require manual grants.
- **Follows established patterns.** ADR-0011 already established console as the single HTTP surface; this is a natural extension of that pattern.
- **Defers cloud-relay complexity.** Phase 2 can introduce a cloud relay without disrupting this design. Local Funnel and cloud relay are alternative binding choices, not competing architectures.
- **Scales to more sources.** Adding a new external source (e.g., Gitea, GitLab, Atlassian automation) requires only a new adapter module in trusty-mpm, not new console binding logic.

### Harder / Trade-offs

- **Tailscale dependency.** Webhook ingestion now requires Tailscale to be installed and the machine to be a member of a tailnet. For air-gapped deployments or users outside Tailscale, a later upgrade path (cloud relay, or direct inbound binding) must be available.
  - Mitigation: Phase 2 documents cloud relay as an escape hatch; the design is not locked to Tailscale.
- **Funnel availability.** Tailscale Funnel is a relatively recent feature (late 2023). Very old Tailscale versions (pre-1.48) do not support it. Users must upgrade Tailscale to use this.
  - Mitigation: documented in setup docs; fallback for old versions is to use a cloud relay or enable direct inbound binding (manually).
- **Network hops.** Webhooks go through Tailscale's relay servers (for non-Tailscale-direct connections), adding latency and potential bandwidth cost. For high-volume webhook scenarios, a cloud relay or direct ingress might be more efficient.
  - Mitigation: expected use case is moderate volume (GitHub issue events, JIRA updates, etc., not high-frequency message streams); if latency becomes critical, phase 2 upgrade path is documented.

### Neutral / Follow-up

- **Console binding logic increases slightly.** Adding Funnel to BindMode adds ~50–100 lines to trusty-console/bind.rs. Complexity is localized.
- **Tailscale auth flow.** Funnel requires Tailscale auth; a user must `tailscale login` and be a member of the configured tailnet. This is an additional step vs. a cloud relay, but it is one-time setup.
- **Phase 2 decision point.** Once real users deploy and feedback arrives, a cloud relay might become necessary. The decision can be revisited then without architectural rework.

## Related Decisions

**Consistency vetting (DOC-46 §3):**

| ADR/Spec | Decision | Verdict | Notes |
|---|---|---|---|
| ADR-0011 | trusty-console is the single HTTP surface | **Consistent / Extends** | This ADR is a direct application of 0011's principle; webhook ingress routes through console's reverse proxy, reinforcing the pattern |
| ADR-0005 | Shared HarnessEvent bus with External payload arm | **Consistent** | External events ingested here flow onto the bus via the new External arm; 0017 does not change the bus design, only specifies ingress transport |
| ADR-0004 | Three harnesses on shared event-driven trusty-common | **Consistent** | External events from external integrations become inputs to trusty-mpm (one of the three harnesses); consistent with the shared foundation model |
| DOC-47 | External event ingestion spec | **Supersedes** | This ADR (0017) captures the load-bearing architectural choices referenced by DOC-47. Both the spec and ADR are normative; ADR documents the "why" and trade-offs; spec is prescriptive |
| DOC-45 | Remote MCP credential delivery | **Consistent** | Webhook signing secrets follow DOC-45's credential model (environment variables, per-integration config, never URL-embedded) |

No conflicts detected. The decision to use shared ingress via console + Tailscale Funnel is compatible with all prior accepted decisions.
