# trusty-console — Architecture Design

**Date:** 2026-06-08
**Status:** Design artifact for human review
**Tracking issues:** #959 (P0 completion), #960 (P1 reverse-proxy)

## Goal

trusty-console is a single long-running HTTP service that serves a unified web dashboard fronting the four core trusty daemons (trusty-memory :7070, trusty-search :7878, trusty-analyze :7879, trusty-review :7880), extensible to more later. It eliminates the need to remember per-daemon ports and open multiple browser tabs. Default port: 7788.

## Current state (P0 scaffold already exists)

`crates/trusty-console/` is already scaffolded and compiles: axum service, embedded Svelte 5 + Vite 6 SPA (via rust-embed), a `ServiceConnector` trait, and discovery for 3 of 4 daemons. publish=false, edition=2024, MSRV 1.91, license.workspace=MIT.

Remaining for P0 (issue #959): add `ReviewConnector` (reads ~/.trusty-review/http_addr), register it in detect/mod.rs all_connectors(); write ~/.trusty-console/http_addr via trusty_common::write_daemon_addr on bind; add .with_graceful_shutdown(trusty_common::shutdown_signal()); replace inline tracing_subscriber with trusty_common::init_tracing(1).

## Daemon HTTP surfaces (ground truth)

| Daemon | Port | Health | Discovery |
|---|---|---|---|
| trusty-search | 7878 | GET /health {status,version,indexes,...} | ~/.trusty-search/http_addr; `trusty-search port` |
| trusty-memory | 7070 | GET /health {status,version,palace_count,...} | ~/.trusty-memory/http_addr; `trusty-memory port` |
| trusty-analyze | 7879 | GET /health {status,search_reachable} | NO discovery file (port assumed 7879) |
| trusty-review | 7880 | GET /health {status,version,inference,deps} | ~/.trusty-review/http_addr |

Embedded UIs: search, memory, analyze each ship a Svelte SPA; review has no UI (REST + webhook only).

## Architecture options

**Option A — Reverse proxy + aggregator (RECOMMENDED).** Console serves its own SPA at `/`, polls each daemon's /health for a status dashboard, and reverse-proxies other traffic to each daemon under /proxy/{daemon}/{*path}. Zero daemon code changes; daemon SPAs work unmodified if served with a correct base href. Single browser URL. Main complexity: path rewriting (inject `<base href="/proxy/{daemon}/">` into proxied index.html, OR set Vite `base` per daemon).

**Option B — BFF.** Console calls each daemon, normalizes, and renders all views in its own SPA. Full UX control and cross-daemon features, but must reimplement every daemon UI (throws away analyze's D3 dashboard). Too heavyweight now.

**Option C — Static aggregator (links/iframes).** Dashboard of link cards to each daemon's own UI. This is essentially today's P0. Simple but fragments UX across tabs/ports — the exact problem the console should solve.

Recommendation: ship C-grade health dashboard as MVP (P0), then move to A (reverse proxy) for P1.

## Recommended crate layout

```
crates/trusty-console/
├── Cargo.toml (exists; publish=false, edition=2024)
├── build.rs (exists; SKIP_UI_BUILD=1 supported)
├── ui/ (Svelte 5 + Vite 6; dist embedded via rust-embed)
└── src/
    ├── main.rs (CLI entry)
    ├── server.rs (axum router; /health, /api/console/services, SPA)
    ├── connector.rs (ServiceConnector trait, ServiceInfo, ServiceStatus)
    ├── detect/ {mod.rs, helpers.rs, search.rs, memory.rs, analyze.rs, review.rs(P1)}
    └── proxy/ {mod.rs, rewrite.rs} (P1)
```

Add `trusty-common = { workspace = true }` for shutdown_signal/write_daemon_addr/init_tracing. P1 proxy needs only `reqwest` (already in workspace deps).

## MVP vs later

- **P0 (issue #959):** health dashboard for all 4 daemons + complete workspace-convention alignment. ~½ day.

  **Operator note:** trusty-analyze writes no discovery file (see issue #956), so the P0 health dashboard assumes port 7879. If trusty-analyze is started on a non-default port, the console will show it as unreachable until #956 lands. Promoting the #956 discovery-file fix into P0 scope would remove this caveat.
- **P1 (issue #960):** /proxy/{daemon}/{*path} reverse-proxy (reqwest streaming) + base-href handling + background health-poll cache + SPA links into proxied views. Watch the 500-line cap on server.rs — split proxy routes + poller into their own modules.
- **P2 (future):** cross-daemon features (unified search bar, memory recall panel, review trigger); optional console_status MCP tool.

## 5 key decisions before P1 (issue #960)

1. Path-rewriting strategy: response-body `<base>` injection (zero daemon changes, fragile) vs Vite `base` config per daemon (clean one-liner, needs coordinated daemon releases). RECOMMENDED: Vite base if daemon changes acceptable.

   **Caveat — response-body injection is genuinely fragile:** it requires buffering the response (breaking streaming and Content-Length), it conflicts with Content-Encoding/compression (proxied HTML compression must be disabled), and it can corrupt pages if `<head>` is split across chunks. Pursue it ONLY if coordinated daemon releases are truly impossible; otherwise prefer the Vite `base` config approach.
2. trusty-analyze discovery file: add write_daemon_addr to trusty-analyze (see issue #956) vs keep hard-coded 7879 fallback.
3. Background health-poll caching: per-request TCP probes (simple, blocks response on 4 probes) vs background tokio task with Arc<RwLock<...>> cache (lower latency). RECOMMENDED: background cache, 15s interval.
4. Proxy implementation crate: reqwest streaming (already a dep) vs hyper direct. RECOMMENDED: reqwest.
5. UI embedding: keep rust-embed (console's existing choice) vs migrate to include_dir! for workspace consistency. Cosmetic.

## Recommended next step

Land P0 (issue #959) first — single PR in crates/trusty-console/src/ adding the ReviewConnector, discovery file, graceful shutdown, and init_tracing. Then resolve the 5 decisions above and implement P1 (issue #960).
