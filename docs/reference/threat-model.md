# HTTP Trust-Boundary Threat Model

This is the authoritative reference for **who can reach which trusty-\* HTTP
surface, from where, and what stops them.** It exists because the
2026-07-19 architecture review (`docs/research/architecture-review-2026-07-19.md`,
Appendix C) found a gap between doctrine (ADR-0011's original "HTTP exactly
once" text) and reality (six daemons besides `trusty-console` each bind their
own HTTP listener) — and because, before this document, **no single place**
recorded which daemon was guarded, which was proxied, and which clients
reached each one directly.

## The doctrine, in one paragraph

**[ADR-0018 — Loopback-only doctrine](../adr/0018-loopback-only-doctrine.md)**
(amending [ADR-0011](../adr/0011-tctl-owns-service-lifecycle.md)) is the
controlling decision: `trusty-console` is the only daemon allowed to bind an
address reachable off-loopback (LAN, Tailscale, Funnel). Every sibling daemon
(search, memory, analyze, review, agents, mpm) may run its own **loopback-only**
HTTP server for CLI commands, MCP stdio-bridge proxying, and same-machine GUI
clients — a supported pattern, not a violation — provided its write-mutating
routes run the shared `trusty_common::server` origin guard. External
reachability is exclusively through console's reverse proxy
(`/api/{service}/*path`) and its Tailscale/Funnel bind modes. See the ADR for
the full context, the rejected full-UDS alternative, and the consistency
vetting against ADR-0011/ADR-0017. This document does not restate that
reasoning — it is the compliance inventory the ADR promises.

## Client inventory

Ports and bind defaults below were verified against `origin/main` at the time
of writing; re-verify line/port references before citing them elsewhere, as
the daemons' listener setup code moves independently of this doc.

| Daemon | Default bind | Origin guard | Console-proxy allowlist | Own UI | Direct clients |
|---|---|---|---|---|---|
| **trusty-search** | `127.0.0.1:7878` (`service/constants.rs::DEFAULT_PORT`) | **Yes** — router-wide `SelfOrigins` guard (`service/server/mod.rs`) | Yes (`search` → `trusty-search`) | Yes, embedded (`ui/`) | CLI (`trusty-search status`/`port`), native MCP stdio bridge (HTTP→`/rpc`), embedded UI, console proxy |
| **trusty-memory** | `127.0.0.1:7070`–`7079` (auto-selects next free); `--http` accepts any explicit addr | **Yes** — router-wide `SelfOrigins` guard (`web/mod.rs`) | Yes (`memory` → `trusty-memory`) | Yes, embedded (`ui/`) | CLI, native MCP stdio bridge — a pure HTTP proxy forwarding JSON-RPC to the daemon's own `/rpc` (no local tool logic), embedded UI, console proxy |
| **trusty-analyze** | `127.0.0.1:7879` (`commands/port.rs::DEFAULT_PORT`) | **Yes** — router-wide `SelfOrigins` guard (`service/routes.rs`) | Yes (`analyze` → `trusty-analyze`) | Yes, embedded (`ui/`) | CLI, native MCP stdio bridge, embedded UI, console proxy, **direct GitHub webhook** (`POST /webhooks/github` — see Known Gaps) |
| **trusty-review** | `127.0.0.1:7891` | **Yes** — `SelfOrigins` guard wraps all routes (`service/routes.rs`) | Yes (`review` → `trusty-review`) | No embedded UI | CLI, console proxy, HMAC-verified GitHub webhook (signature check is independent of the origin guard) |
| **trusty-agents** | **`0.0.0.0:7654`** (LAN-reachable by default) — optional bearer auth, **off by default** | **No** | **No** — not yet in the allowlist | Yes, separate `agents-ui` crate | CLI, native MCP stdio bridge, `agents-ui` (Tauri, talks directly to the `0.0.0.0` API), any host on the LAN (this is the gap) |
| **trusty-mpm** | `127.0.0.1:7880` | **Yes** — `guard_router` wraps the listener with the `SelfOrigins` guard (`daemon/api/origin_guard.rs`) | Yes (`mpm` → `trusty-mpm`, #1849 Phase 1) | No embedded UI (separate `trusty-mpm-gui` Tauri app) | CLI (`tm`), native MCP stdio bridge, `trusty-mpm-gui` (talks **directly** to the daemon, not gateway-first — migration tracked as [#3333](https://github.com/bobmatnyc/trusty-tools/issues/3333), #1849 Phase 2), console proxy |
| **trusty-console** | `127.0.0.1:7788` by default; `--tailscale` widens to a dual listener (loopback + tailnet IP); Funnel mode (ADR-0017, Proposed) layers public HTTPS on top | Yes — the original router-wide guard this pattern was lifted from (`crates/trusty-common/src/server/origin_guard.rs` docs the provenance: #3268/#3269/#3280) | n/a — console is the proxy, not a proxied target (`full_id("console")` is explicitly `None`) | Yes, the dashboard SPA | Browsers (local or, in `--tailscale`/Funnel mode, remote), every CLI/UI above via the reverse proxy, and — the **sole off-loopback surface** per ADR-0018 |

**One item above is still in progress:** trusty-agents'
bind/guard/allowlist fix (#3329, epic #3328). The trusty-mpm `--tailscale`
listener removal (#3330) and trusty-review origin guard (#3332) are resolved in
this version.

## Why the guard fails open on a missing `Origin` header

The shared guard (`trusty_common::server::guard_write_origin`,
`crates/trusty-common/src/server/origin_guard.rs`) rejects a write request
only when an `Origin` header is **present** and does not match a loopback or
self-bound origin. A request with **no** `Origin` header is allowed through.
This is deliberate, not an oversight:

- **Browsers always send `Origin`** on cross-origin state-changing requests
  (the CSRF threat this guard defends against) — so the guard's coverage of
  the actual threat model is complete without needing an `Origin` on every
  request.
- **Server-to-server calls send no `Origin`:** the console's reverse proxy
  (a `reqwest` client forwarding to the backing daemon) does not set one,
  nor does a daemon's own internal HTTP client.
- **`curl` and other CLI tooling** send no `Origin` by default — operators
  running raw HTTP commands against their own loopback daemon are not the
  CSRF threat.
- **Native MCP stdio bridges** are plain HTTP clients (not browsers) forwarding
  JSON-RPC to `/rpc` — no `Origin` header in that path either.
- **Webhook senders** (GitHub, and any future signed-webhook source) send no
  `Origin` header; their authenticity is established by a signature check
  (HMAC for trusty-review, a comparable mechanism for trusty-analyze), which
  is a separate control from this guard and is not weakened by the guard's
  fail-open behavior.

Fail-closed on a missing `Origin` would break every one of those legitimate,
non-browser callers. The guard's actual job is narrower and correctly scoped:
stop a browser, already holding session locality, from being tricked into
firing a cross-origin write.

## Known gaps

- **ADR-0017's webhook-ingress gap.** [ADR-0017](../adr/0017-shared-ingress-via-console-tailscale-funnel.md)
  (Proposed) plans a single `/api/webhooks/{source}` endpoint mounted in
  trusty-mpm and reverse-proxied by console — but it does not mention
  retiring trusty-analyze's existing **direct** `POST /webhooks/github`
  endpoint (`crates/trusty-analyze/src/service/routes.rs`). Today that route
  is reachable only on trusty-analyze's own loopback bind (consistent with
  the loopback-only doctrine), but it is a second, unproxied webhook path
  that predates ADR-0017 and isn't addressed by it. Flagged here for whoever
  lands ADR-0017: decide whether analyze's webhook route is retired in favor
  of the new shared endpoint, or documented as a deliberate second path.
- **Console strips the `Upgrade` header.** The reverse proxy's hop-by-hop
  header list (`crates/trusty-console/src/proxy/routes.rs:37`,
  `HOP_BY_HOP`) includes `"upgrade"` alongside the standard RFC 7230 §6.1 set.
  This is correct proxy hygiene for todays's plain-HTTP backends, but it means
  a future WebSocket endpoint added to any sibling daemon will silently fail
  to proxy through console — the `Upgrade: websocket` handshake header never
  reaches the backend. Latent, not yet triggered: no daemon currently exposes
  a WebSocket route. Whoever adds the first one needs to special-case the
  proxy to pass `Connection`/`Upgrade` through for that route.
- **trusty-agents' undeclared 7th HTTP surface.** `crates/trusty-agents/src/search/service/mod.rs`
  runs a per-project search-as-a-service daemon in the background (PID
  tracked at `.trusty-agents/state/search.pid`). It does not appear in any
  daemon inventory, port table, or (until this document) threat model; it is
  not currently subject to the loopback-only doctrine's enforcement and is
  not wrapped by the shared write-origin guard. Tracked as
  [#3335](https://github.com/bobmatnyc/trusty-tools/issues/3335) — needs
  either formal declaration (loopback bind + guard) or retirement.

## Credential delivery (not this document's scope)

Secrets and credential hygiene for the installer and for daemon-to-daemon
calls are governed by [`SECURITY.md`](../../SECURITY.md) at the repo root —
see it for the installer trust model and dependency-security policy; this
document does not restate it. Credential delivery specifically for **remote
MCP servers injected into fleet sessions** (the OAuth/static-env/`headersHelper`
preference chain and URL-embedded-secret detection) is being formalized as a
dedicated spec (tracked by issue [#3038](https://github.com/bobmatnyc/trusty-tools/issues/3038),
not yet merged); once that spec lands, this section should link it directly
instead of the issue.
