# HTTP Trust-Boundary Threat Model

🔴 **This document's per-daemon inventory is superseded and needs revision.**
[ADR-0032](../adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)
(2026-08-07 owner ruling) reverses the premise this table is built on: no
sibling daemon runs its own HTTP server at all, loopback-only included —
inter-service traffic moves to UDS, and only `trusty-console` keeps HTTP.
[ADR-0018](../adr/0018-loopback-only-doctrine.md), the doctrine this document
was written to audit compliance against, is now `Superseded by 0032`. The
bind/guard/console-proxy-allowlist table below still describes the **live**
topology as of this writing for five of the six daemons in ADR-0032's Scope
— it is no longer a description of the target architecture, and most of its
columns (per-daemon bind address, origin guard) stop applying once a daemon
has no HTTP listener to guard. Re-deriving those rows for the UDS topology is
implementation work, out of scope for this update. Read ADR-0032 before
treating any row below as prescriptive.

> 🟡 **Progress note (2026-08-26, Refs #6277, PR #6281).** `trusty-review` is
> no longer part of "nothing has migrated": it is the first daemon through
> ADR-0032's path, and its row below already reflects the UDS socket it now
> serves. The remaining five daemons (`trusty-search`, `trusty-memory`,
> `trusty-analyze`, `trusty-agents`, `trusty-mpm`) have not migrated and
> their rows still describe the pre-ADR-0032 live topology.

This is the authoritative reference for **who can reach which trusty-\* HTTP
surface, from where, and what stops them.** It exists because the
2026-07-19 architecture review (`docs/research/architecture-review-2026-07-19.md`,
Appendix C) found a gap between doctrine (ADR-0011's original "HTTP exactly
once" text) and reality (six daemons besides `trusty-console` each bind their
own HTTP listener) — and because, before this document, **no single place**
recorded which daemon was guarded, which was proxied, and which clients
reached each one directly.

## The doctrine, in one paragraph

🔴 **Read this first — the paragraph below documents ADR-0018, which is
superseded.** The live design principle is
[ADR-0032's Design Principle section](../adr/0032-no-service-owns-http-console-is-the-only-http-surface.md#design-principle):
every trusty-\* service is a fast local service that speaks UDS, and
`trusty-console` is the one shared daemon that extends any of them to the
web over HTTP. Start a new service, or add a capability to an existing one,
by reading that paragraph before this section's ADR-0018 history.

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

**[ADR-0031 — transport by purpose](../adr/0031-uds-for-inter-crate-transport-http-for-external.md)**
(Proposed) sits on a different axis and does **not** amend this doctrine.
ADR-0018 governs bind-address reachability; ADR-0031 governs which local
transport inter-crate callers use to reach an already-loopback-bound daemon —
UDS for inter-crate same-host traffic, one shared HTTP server for everything
external. **Migration has started but is not complete** — `trusty-review` has
moved (PR #6281, see the progress note above); the inventory below still
describes the live topology for the other five daemons. Whatever HTTP remains
under ADR-0031 stays governed by ADR-0018 exactly as written, and ADR-0031
authorises no new off-loopback binding. If adopted it would *strengthen* this
doctrine's goal: a loopback TCP port is reachable by any local process, a
`0600` socket is not. See ADR-0031 for the classification test.

**Two things this table does not show, worth stating so it is not misread:**

1. **It is an inventory of HTTP surfaces, not of the whole transport topology.**
   One daemon has no HTTP surface and therefore no row: **trusty-embedderd**
   (Unix socket + stdio). Its absence is correct here, not an omission.
   #5329 removed a second, **trusty-bm25-daemon** — its per-palace Unix-socket
   JSON-RPC surface no longer exists at all, because the index it guarded now
   runs inside trusty-memory.
2. **Each daemon's stdio/MCP surface is a client of its own HTTP daemon, not a
   separate surface.** `trusty-memory serve --stdio` is a pure proxy to
   `POST /rpc` and never opens redb (#1078); trusty-search's and
   trusty-analyze's MCP servers are HTTP clients of theirs. So the "native MCP
   stdio bridge" listed among the direct clients below reaches the daemon
   through the same loopback port as everything else, and inherits the same
   guard. If ADR-0031's tool paths later move off HTTP, these rows will need to
   distinguish a daemon's remaining management routes from its tool routes.

## Client inventory

Ports and bind defaults below were verified against `origin/main` at the time
of writing; re-verify line/port references before citing them elsewhere, as
the daemons' listener setup code moves independently of this doc.

| Daemon | Default bind | Origin guard | Console-proxy allowlist | Own UI | Direct clients |
|---|---|---|---|---|---|
| **trusty-search** | `127.0.0.1:7878` (`service/constants.rs::DEFAULT_PORT`) | **Yes** — router-wide `SelfOrigins` guard (`service/server/mod.rs`) | Yes (`search` → `trusty-search`) | Yes, embedded (`ui/`) | CLI (`trusty-search status`/`port`), native MCP stdio bridge (HTTP→`/rpc`), embedded UI, console proxy |
| **trusty-memory** | `127.0.0.1:7070`–`7079` (auto-selects next free); `--http` accepts any explicit addr | **Yes** — router-wide `SelfOrigins` guard (`web/mod.rs`) | Yes (`memory` → `trusty-memory`) | Yes, embedded (`ui/`) | CLI, native MCP stdio bridge — a pure HTTP proxy forwarding JSON-RPC to the daemon's own `/rpc` (no local tool logic), embedded UI, console proxy |
| **trusty-analyze** | `127.0.0.1:7879` (`commands/port.rs::DEFAULT_PORT`) | **Yes** — router-wide `SelfOrigins` guard (`service/routes.rs`) | Yes (`analyze` → `trusty-analyze`) | Yes, embedded (`ui/`) | CLI, native MCP stdio bridge, embedded UI, console proxy. **No webhook surface** — `POST /webhooks/github` was retired in #5181 and 404s |
| **trusty-review** | **No TCP listener.** Serves `<data dir>/trusty-review.sock` (`trusty_common::daemon_socket_path`), bound through `bind_singleton_hardened` (#6277, ADR-0032) | **n/a** — `SelfOrigins`/`with_guarded_middleware` deliberately not ported: browser-CSRF machinery has no meaning on a socket. The boundary is the `0700` directory, the `0600` socket, and `ensure_peer_is_self` on every accepted connection | **n/a** — nothing to proxy; ADR-0035 aggregator routing is deferred | No embedded UI | CLI, `trusty-console`'s `ReviewConnector`, `tctl`'s health probe — all over that socket. **No webhook surface** — `POST /pr/github/webhook` was retired in #5181; the webhook path is the separate `trusty-review-webhook.sock` (ADR-0034) |
| **trusty-agents** | `127.0.0.1:8080` (`--port`; 7654 is the conventional dev/UI port, passed explicitly). `--bind` is an explicit non-loopback opt-in that `serve_with_config` **refuses to start without `--api-token`** (`api/server/routes.rs`) | **Yes** — router-wide `SelfOrigins` guard via `with_guarded_middleware` (`api/server/routes.rs::build_router_with_origins`) | Yes (`agents` → `trusty-agents`, #3331) | Yes, separate `agents-ui` crate | CLI, native MCP stdio bridge, `agents-ui` (Tauri — writes go over Tauri IPC, not HTTP), console proxy |
| **trusty-mpm** | `127.0.0.1:7880` | **Yes** — `guard_router` wraps the listener with the `SelfOrigins` guard (`daemon/api/origin_guard.rs`) | Yes (`mpm` → `trusty-mpm`, #1849 Phase 1) | No embedded UI (separate `trusty-mpm-gui` Tauri app) | CLI (`tm`), native MCP stdio bridge, `trusty-mpm-gui` (talks **directly** to the daemon, not gateway-first — migration tracked as [#3333](https://github.com/bobmatnyc/trusty-tools/issues/3333), #1849 Phase 2), console proxy |
| **trusty-code** | `127.0.0.1:7882` (`serve/mod.rs::DEFAULT_HTTP_PORT`; `tcode serve --http`, auto-spawned by `tcode tui`) | **Yes** — router-wide `SelfOrigins` guard plus same-origin CORS via `with_guarded_middleware_same_origin_cors` (`serve/http.rs::build_axum_router`, #6003). It took the permissive-CORS stack with no guard until then | No — no `code` key in `full_id`, so `/api/code/*` is not proxied | No embedded UI (separate `trusty-code-gui` Tauri app) | `tcode tui`'s own HTTP client and the CodeEngine adapter (daemon located by `TCODE_DAEMON_URL`, else the `http_addr` discovery file), `trusty-code-gui` (talks **directly** to the daemon over HTTP, like `trusty-mpm-gui`). Caller authentication and the HTTP-vs-UDS transport question are open under [#5439](https://github.com/bobmatnyc/trusty-tools/issues/5439) / ADR-0032 |
| **trusty-console** | `127.0.0.1:7788` by default; `--tailscale` widens to a dual listener (loopback + tailnet IP); Funnel mode (ADR-0017, Proposed) layers public HTTPS on top | Yes — the original router-wide guard this pattern was lifted from (`crates/trusty-common/src/server/origin_guard.rs` docs the provenance: #3268/#3269/#3280) | n/a — console is the proxy, not a proxied target (`full_id("console")` is explicitly `None`) | Yes, the dashboard SPA | Browsers (local or, in `--tailscale`/Funnel mode, remote), every CLI/UI above via the reverse proxy, and — the **sole off-loopback surface** per ADR-0018 |

**Every row above is now resolved.** trusty-agents' bind/guard/allowlist fix
(#3329, epic #3328) landed in [#3341](https://github.com/bobmatnyc/trusty-tools/pull/3341)
together with the console `agents` proxy route (#3331); the trusty-mpm
`--tailscale` listener removal (#3330) and trusty-review origin guard (#3332)
were resolved earlier.

**Remote access to trusty-agents goes through trusty-console**, not through a
direct bind. Per [ADR-0018](../adr/0018-loopback-only-doctrine.md) console is
the sole off-loopback surface and the only component that widens over
Tailscale, and [ADR-0031](../adr/0031-uds-for-inter-crate-transport-http-for-external.md) routes all
external comms through the one shared HTTP server. trusty-agents publishes its
bound address to the standard `http_addr` discovery file so the console proxy
resolves `/api/agents/*` to it. The `--bind` escape hatch remains for the case
console cannot cover, which is why it is token-gated at startup rather than
merely discouraged.

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

- ~~**ADR-0017's webhook-ingress gap.**~~ **CLOSED (#5181).**
  [ADR-0034](../adr/0034-webhook-ingress-console-relays-over-uds-to-a-supervised-on-demand-process.md)
  settled it: `trusty-console`'s `POST /api/webhooks/{source}` is the only HTTP
  webhook surface in the workspace and the only holder of the shared secret. It
  verifies the HMAC once, spools the payload durably, and relays over UDS.
  Both direct routes — trusty-analyze's `POST /webhooks/github` and
  trusty-review's `POST /pr/github/webhook` — are deleted and now 404, so
  neither crate verifies a signature or exposes a second, unproxied path.
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
  daemon inventory, port table, or (until this document) threat model. It does
  bind loopback — `TcpListener::bind("127.0.0.1:0")`, hardcoded, so there is no
  configuration that widens it — but that is incidental rather than enforced,
  and it is not wrapped by the shared write-origin guard. Tracked as
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
