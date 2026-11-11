# Cloud sled network surface audit (G0b)

Date: 2026-11-11. Goal: confirm that all internet networking goes through one
hardened surface, trusty-console. Read-only audit. Refs #11054.

## Summary

1. The goal does not hold today. The console is an inbound surface only; it makes no outbound internet call, and about 25 outbound paths in 14 crates reach the internet directly.
2. Inbound is close. Every daemon is UDS-only, except one: trusty-agents (`tagent --api`) binds its own HTTP port (default `127.0.0.1:8080`), and `--bind` with a token accepts any IP.
3. The console has no TLS and no token. Off-loopback it trusts only same-login tailnet peers. On loopback it trusts any caller that sends the right `Host`, including the MagicDNS name, so a `tailscale funnel` in front of it would be public and unauthenticated (inferred).
4. No "cloud sled" spec exists in this repo. The only design artifacts are the cloudtest container (#11054) and its squid egress allowlist. That allowlist is the workable pattern for outbound.
5. Recommendation: treat the console as the single inbound surface and a network egress proxy (squid allowlist) as the single outbound surface. Disable update checks, model and uv downloads, chat sockets and `tagent --api` on a sled until Bob rules on each.

## Sled design found

- No spec, ADR or issue in trusty-tools defines a "cloud sled". `rg -i '\bsleds?\b'` finds only the unrelated `sled` KV crate (`docs/specs/trusty-headroom.md:225`, `crates/trusty-agents/src/listeners/store.rs:9`).
- #11054 itself says: "No existing issue or spec covers a container or cloud deploy of the stack."
- The architect briefs name "Cloud Dev Sleds: EKS-Based Developer Environments", an engineering-platform/devops initiative (`architect/local/briefs/2026-10-14-apex-3376-backfill-proposal.md:281`, `2026-10-25-dep007-bet-backfill-list.md:122`). That is a Duetto platform initiative. Its networking model is not in these files. Inferred: a sled is an EKS pod, so other pods and sidecars may share the network namespace.
- The design docs that do govern networking:

| Doc | What it assumes about networking |
|---|---|
| ADR-0032 (`docs/adr/0032-...md:41-59`) | No trusty-* service binds HTTP; UDS between services; the console is the one HTTP surface. It covers inbound HTTP only and says nothing about outbound calls. |
| ADR-0031 (`docs/adr/0031-...md:1-22`) | Same-host traffic over UDS; external traffic through one shared HTTP server. Amended by 0032. |
| ADR-0017 (`docs/adr/0017-...md:1-76`) | Status Proposed. Public webhook ingress through the console via Tailscale Funnel, with a planned `BindMode::Funnel`. Not built: `BindMode` has only Local, Tailscale and Explicit (`crates/trusty-console/src/bind.rs:29-36`). |
| ADR-0034 | The console terminates webhooks, verifies HMAC once, spools them, and relays them over UDS. |
| `docker/cloudtest/README.md:56-123` | Rung 2: the app container sits on an `--internal` network; a squid container allows CONNECT :443 to `api.anthropic.com`, `platform.claude.com` and `claude.ai` only (`docker/cloudtest/proxy/squid.conf:13-24`); `HTTPS_PROXY` is set in the image (`docker/cloudtest/Dockerfile:84`). |
| tm sandbox policy (`crates/trusty-mpm/src/core/session_launch/sandbox_settings.rs:68-75`) | The Claude Code sandbox allows Bash to reach github.com, api.github.com, crates.io, static.crates.io, index.crates.io and registry.npmjs.org. The Architect runs unsandboxed (`:31-33`). It governs sandboxed Bash only, not the daemons. |

## Listeners table

TCP and UDP listeners in production code (7):

| # | Crate | file:line | Default addr | Configurable | Auth | Internet-facing? | Via console? |
|---|---|---|---|---|---|---|---|
| L1 | trusty-console | `src/bind.rs:224`, served by `src/tailnet_peer/mod.rs:369-386` | `127.0.0.1:7788` (`src/lib.rs:38,184`) | `--http`, `TRUSTY_CONSOLE_BIND` (`src/bind.rs:55,80-103`) | No token. Host/Origin check only (`src/tailnet_peer/loopback_origin.rs:1-15`); a MagicDNS `Host` is admitted once learned | Only through a fronting proxy: `tailscale serve` (tailnet) or a funnel (public, inferred) | It is the console |
| L2 | trusty-console | same bind; `src/tailnet_peer/mod.rs:294-316,389-397` | off; `--tailscale` adds `<ts-ip>:7788` (`src/bind.rs:128-146`) | `--tailscale`, `--http <ip:port>`, env | Tailnet peer gate: same login, untagged node, `tailscale whois` (`src/tailnet_peer/mod.rs:1-24`). A wildcard bind refuses every request (`:389-395`). Plain HTTP, no TLS | Tailnet only. A non-tailnet peer is always refused | It is the console |
| L3 | trusty-agents | `src/api/server/routes.rs:737,796` | `127.0.0.1:8080` (`src/runtime/mode_dispatch.rs:205`, `src/api/server/auth.rs:60-65`) | `--bind <ip>`, `--port`, `--api-token`, `TAGENT_API_TOKEN` (`src/runtime/startup.rs:383-406`) | None on loopback. A non-loopback bind needs a bearer token (`routes.rs:610-617`). No TLS | Yes, if started with `--bind 0.0.0.0` and a token. Prints a LAN URL (`routes.rs:776-781`) | Partly: it writes `http_addr` so the console proxies `/api/agents/*` to it over HTTP (`routes.rs:766-772`), but the port stays directly reachable |
| L4 | trusty-agents | `src/api/server/routes.rs:826` | UDP `0.0.0.0:0` | no | n/a | No. It "connects" to 8.8.8.8:80 to learn the LAN IP; no packet is sent. Only runs on a non-loopback bind | n/a |
| L5 | trusty-code-gui | `src/bridge/serve.rs:65,83` | `127.0.0.1:0` | no | Per-launch minted token on every route (`serve.rs:47-55`) | No | No (desktop exception, ADR-0032 addendum) |
| L6 | trusty-gworkspace | `src/api/auth/oauth/callback.rs:35` | `127.0.0.1:0`, during OAuth login only | no | PKCE (`callback.rs:23`) | No | No |
| L7 | trusty-installer | `src/commands/port_guard.rs:504` | `127.0.0.1:<port>`, bound then dropped as a probe | no | n/a | No | n/a |

Unix-socket listeners (13), all through `trusty_common::uds::bind_hardened` / `bind_singleton_hardened` (0600 socket, 0700 dir, `SO_PEERCRED` same-uid check; `crates/trusty-common/src/lib.rs:1396-1412`):

| Crate | file:line |
|---|---|
| trusty-mpm (tm daemon) | `src/daemon/socket.rs:140` |
| trusty-search | `src/service/socket.rs:440` |
| trusty-memory | `src/transport/uds.rs:492` |
| trusty-embedderd | `src/uds_server.rs:50` |
| trusty-analyze | `src/service/rpc.rs:629` |
| trusty-events | `src/serve.rs:143` |
| trusty-secrets | `src/server/router.rs:707` |
| trusty-code | `src/serve/uds.rs:302` |
| trusty-console (own UDS) | `src/console_uds/mod.rs:174` |
| trusty-common webhook relay target | `src/webhook_relay/listener.rs:195` |
| trusty-agents bus | `src/bus/mod.rs:82` |
| trusty-agents ctrl | `src/ctrl/socket.rs:268,303` |
| trusty-agents search rpc | `src/search/service/rpc.rs:240` |

None of the UDS listeners can face the network. Test-only binds (about 40 hits in `#[cfg(test)]` modules and `*_test*.rs`) are excluded.

## Outbound table

"Proxy" means it honours `HTTPS_PROXY`. reqwest 0.12 reads env proxies by default (workspace `Cargo.toml:236`). Only the loopback builders opt out (`crates/trusty-common/src/http_client.rs:71,99`), and they are used only for loopback daemon calls. Per-site proxy behaviour was not run; it is inferred from the client library. No path goes through the console, because the console makes no outbound internet call: its reqwest clients target loopback daemons only (`crates/trusty-console/src/server/mod.rs:224-232`, `src/proxy/routes.rs`).

| # | Crate | file:line | Destination | Trigger | Proxy |
|---|---|---|---|---|---|
| O1 | trusty-common inference | `src/inference/providers/anthropic/mod.rs:39`, `openai.rs:23`, `fireworks.rs:24`, `together.rs:25`, `atlascloud.rs:22`, `chat/openai_compat/providers.rs:30`, `openrouter_legacy.rs:14` | Anthropic, OpenAI, Fireworks, Together, AtlasCloud, OpenRouter | Any LLM call by tcode, tagent, review, memory | yes (reqwest) |
| O2 | trusty-mpm | `src/daemon/llm_overseer.rs:24`, `src/core/sm/providers/openrouter.rs:24`, `src/core/sm/providers/anthropic.rs:30,136` | OpenRouter, Anthropic (`ANTHROPIC_BASE_URL` override) | Overseer and session-manager LLM calls | yes |
| O3 | trusty-agents | `src/llm/adapter/impls.rs:152-153`, `src/workflow/resolver.rs:35`, `src/context/indexer.rs:141` | Anthropic, OpenRouter (chat and embeddings) | Agent turns, context indexing | yes |
| O4 | trusty-memory | `src/engine/semantic_consolidation/inference.rs:21`, `types.rs:218` | OpenRouter, or local Ollama `localhost:11434` | Dream consolidation | yes |
| O5 | trusty-review | `src/llm/openrouter.rs:38`, `src/llm/fireworks.rs:59`, `src/integrations/github/pr.rs:98`, `context/jira.rs:120`, `context/confluence.rs:112` | OpenRouter, Fireworks, api.github.com, Jira, Confluence | A review run | yes |
| O6 | trusty-analyze | `src/core/github.rs:89,128`; update check `src/main.rs:457` | api.github.com, crates.io | PR analysis; CLI start | yes |
| O7 | Claude Code subprocess | tm `src/control/backend/stream_json.rs:98`; tagent `src/agents/claude_code_runner/mod.rs:57` | api.anthropic.com (plus platform.claude.com and claude.ai for login and refresh, `docker/cloudtest/README.md:112-113`), MCP servers, WebFetch, Bash tools | Every PM, agent and claude-code-runner session | yes (Node CLI; shown working through squid in cloudtest rung 2) |
| O8 | trusty-mpm Slack | `src/slack/mod.rs:131,789`, `src/channels/slack_bot.rs:24`, `src/channels/socket_mode/transport.rs:192` | Slack Web API, Socket Mode `wss://` | Chat channel enabled | REST yes; websocket **no** (tokio-tungstenite `connect_async` opens a direct TCP connection) |
| O9 | trusty-agents Slack | `src/slack/mod.rs:233,338`, `src/slack/send.rs:39` | Slack REST and `wss://` | Slack channel bound | REST yes; websocket no |
| O10 | trusty-channels | `src/telegram/api/constants.rs:19`, `client.rs:74`; `src/slack/api/constants.rs:17`; `src/gchat/api/constants.rs:12` | api.telegram.org, Slack, Google OAuth / Chat | tm and tagent channel gateways (`trusty-agents/src/channels/telegram.rs:24,136`) | yes |
| O11 | trusty-gworkspace, trusty-connectors | `trusty-gworkspace/src/api/constants.rs:15,17`; `trusty-connectors/src/live.rs:29` | Gmail, Calendar, Google APIs, tokeninfo | MCP connector tools | yes |
| O12 | tc-services | `src/granola.rs:20` | public-api.granola.ai | Granola sync | yes |
| O13 | trusty-common tickets, trusty-agents ticketing | `trusty-common/src/tickets/api/backends/github/types.rs:18-19`, `linear/types.rs:16`, `jira/client.rs:52`; `trusty-agents/src/ticketing/github/mod.rs:22`, `linear.rs:18`, `jira.rs:77` | GitHub REST/GraphQL, Linear, Jira | Ticket tools | yes |
| O14 | trusty-agents Izzie tools | `src/tools/izzie/weather.rs:25-26` | open-meteo.com | Weather tool | yes |
| O15 | trusty-mpm GitHub | `src/daemon/bug_report/github_client.rs:23`, `src/content/release_source.rs:344-345` | api.github.com, github.com releases | Bug report, content fetch | yes |
| O16 | trusty-mpm `gh` | `src/session_manager/worktree_reclaim_gh.rs:125`, `src/supervisor/pr_cleanup_tick.rs:153` | GitHub via `gh` | Supervisor PR-cleanup tick, worktree reclaim | yes (gh is Go; inferred) |
| O17 | `git` network ops | tm `src/daemon/managed_routes/inproject_hygiene.rs:664`, `inproject_start_point.rs:152`, `inproject/base_clone.rs:180`; trusty-common `src/catchup/session_refs.rs:451`; tagent `src/git/remote.rs:23,35` | Git remotes (GitHub) | Managed-route fetch and clone, catch-up, tagent push and pull | yes (git honours `https_proxy`; inferred) |
| O18 | Update checks | trusty-common `src/update/mod.rs:255`, callers `trusty-memory/src/startup_tasks.rs:213`, `trusty-search/src/commands/start/daemon.rs:655`; tagent `src/update/mod.rs:13,55` | crates.io API, GitHub releases | **Daemon start** (memory, search), tagent REPL | yes. Off with `TRUSTY_NO_UPDATE_CHECK` or `CI` (`update/mod.rs:29,335-340`) |
| O19 | Upgrade and install | trusty-common `src/update/upgrade.rs:195` (`cargo install`); tm `src/bin/tm/commands/reinstall.rs:280`; installer `src/download/release.rs:48,56-57`, `src/commands/up/os_env.rs:100,113,119` (`npm`, `claude update`); publish-guard `src/fetch.rs:42,59` | crates.io, GitHub releases, npm registry, Claude Code updater | Operator `upgrade` / `tctl up` / publish | yes (cargo, npm; inferred) |
| O20 | Embedding models | trusty-common `src/embedder/fast_embedder.rs:387,600`; trusty-agents `src/memory/embed.rs:91`; hf-hub `trusty-common/src/embedder/candle_embedder.rs:130`, `trusty-search/src/service/candle_embedder.rs:77` | Hugging Face | First embed with an empty cache | inferred unknown: hf-hub 0.3 sync uses ureq 2.12 (`Cargo.lock:4260-4273`), which reads env proxies only with its `proxy-from-env` feature; not verified |
| O21 | trusty-embedderd-py | `src/uv_fetch.rs:276`, `src/uv_fetch/http.rs:35,67`; `src/bootstrap.rs:550,559,587` | github.com (uv binary), then uv fetches Python and PyPI wheels (inferred) | `TRUSTY_EMBEDDER=python` first start | yes (comment at `http.rs:35`). Off with `TRUSTY_UV_FETCH=0` (`uv_fetch.rs:240-246`) |
| O22 | Tailscale CLI | console `src/bind.rs:169`, `src/tailnet_peer/whois.rs:83` | local tailscaled (which itself reaches the Tailscale control plane and DERP) | `--tailscale`, every non-loopback request, MagicDNS lookup | n/a (local socket) |
| O23 | Local model hosts | trusty-common `src/inference/providers/local.rs:57`, `src/local_probe.rs:84`; trusty-search `src/service/config.rs:70` | Ollama `localhost:11434` | Local provider | loopback |

Count: 23 outbound groups covering about 60 call sites. Sampled; see Method.

## Gaps and fixes, ranked

Risk is for a cloud host where other tenants or pods may share the network.

| Rank | Gap | Risk | Proposed fix |
|---|---|---|---|
| 1 | Outbound bypasses the console entirely (O1-O21). Nothing in code limits destinations. | High: any prompt-injected agent or tool can reach any host | **Egress proxy allowlist** at the network layer, the squid pattern in `docker/cloudtest/proxy/squid.conf`, with the app on an `--internal` network. Code changes are not required; reqwest, git, gh, cargo and claude honour `HTTPS_PROXY`. |
| 2 | Console loopback listener trusts any caller with a matching `Host`, MagicDNS name included (`loopback_origin.rs:8-13`); no token. A funnel in front of it would admit the public internet to every route, including `/api/agents/*` (inferred: funnel uses the same MagicDNS name). In a shared pod network namespace, any sidecar reaches it. | High | Add a console token, required for every non-UDS caller. Refuse a funnel Host until ADR-0017 is built with per-route exposure (webhooks only). |
| 3 | `tagent --api` binds its own HTTP port (L3), against ADR-0032. It spawns subprocesses. `--bind 0.0.0.0` with a token makes it internet-facing over plain HTTP. | High | **Disable on sled**, or move it to UDS behind the console. At minimum, refuse non-loopback `--bind` on a sled. |
| 4 | Console has no TLS (no rustls or TLS config in `crates/trusty-console`). The non-loopback path is tailnet-only, so a non-Tailscale cloud host has no safe remote path. | Medium | Terminate TLS in front of the console: `tailscale serve`, or an ingress controller with mTLS/OIDC. Keep the console bound to loopback. |
| 5 | Claude Code sessions (O7) reach Anthropic, MCP servers and any Bash or WebFetch target. The tm sandbox allowlist covers sandboxed Bash only; the Architect runs unsandboxed (`sandbox_settings.rs:31-33`). | Medium | Same egress proxy. Allow `api.anthropic.com` for steady state; allow `platform.claude.com` and `claude.ai` only for login. |
| 6 | Update checks run at daemon start (O18) and contact crates.io and GitHub with no operator action. | Medium (info leak, supply-chain prompt) | **Disable on sled**: set `TRUSTY_NO_UPDATE_CHECK=1` in the image. |
| 7 | Model and uv downloads at first use (O20, O21). hf-hub proxy support is unverified, so the call may fail or go direct. | Medium | Bake models into the image or mount them read-only (as cloudtest does with `~/.cache/fastembed`); set `TRUSTY_UV_FETCH=0`; set `HF_HUB_OFFLINE=1` (inferred to be honoured by hf-hub). |
| 8 | Slack Socket Mode websockets (O8, O9) ignore the proxy. Behind squid they fail; without squid they go direct. | Medium | **Disable chat on sled** until ruled on; or add CONNECT-proxy support to the websocket dial and allowlist the Slack hosts. |
| 9 | tm supervisor runs `gh` and `git fetch/clone` on timers (O16, O17). | Low-Medium | Allowlist `github.com` and `api.github.com` only, or turn the PR-cleanup tick off on sled. |
| 10 | Other SaaS tools (O10-O14): Telegram, Google, Granola, Linear, Jira, open-meteo. | Low on a sled if unused | Off by default on sled; allowlist per tool when enabled. |
| 11 | Loopback-only desktop listeners (L5, L6) and the UDP probe (L4). | Low | Not used headless; leave as is. |

## What the console needs

To be "the single hardened surface", the console lacks:

1. **TLS.** No TLS code in the crate. It relies on Tailscale for transport encryption.
2. **Authentication.** No token or bearer check on any route (`rg -i bearer crates/trusty-console/src` finds nothing outside tests). Loopback and MagicDNS callers are trusted on `Host` alone.
3. **A non-Tailscale remote path.** Off-loopback, only same-login tailnet peers pass (`tailnet_peer/mod.rs:1-24`); a wildcard bind refuses everything.
4. **Public webhook exposure.** ADR-0017's `BindMode::Funnel` is not built (`bind.rs:29-36`). The HMAC-verified `/api/webhooks/{source}` route exists (`src/lib.rs:530`, `src/webhook/mod.rs:361`), but nothing limits a public front to that one route.
5. **An MCP route.** The router has `/api/{service}/{*path}`, `/tools/*`, `/ui`, `/api/secrets` and webhooks; no MCP endpoint (`src/server/router.rs`). Remote MCP clients have no path.
6. **An egress role.** The console makes no outbound internet call, so it cannot be the outbound surface without new code. A network egress proxy is simpler.
7. **Closing the tagent HTTP bypass.** The console proxies `/api/agents/*` to tagent's own port instead of over UDS (`trusty-agents/src/api/server/routes.rs:766-772`).

Not checked: rate limiting, request size limits on public routes, and access logging.

## Questions for Bob

1. **Scope of "all networking".** Is the console meant to be the single surface for inbound only, with a network egress allowlist for outbound? Or must outbound also pass through the console, which needs new proxy code?
2. **Tailnet.** On a sled, is Tailscale the only remote path to the console? Or must it work behind a cloud ingress (ALB/EKS ingress) with its own TLS and auth?
3. **Model API calls.** Direct to the provider through an egress allowlist, or through the console as a model gateway? Which providers are allowed: Anthropic only, or also OpenRouter, Fireworks, Together and AtlasCloud?
4. **Package and model registries.** Are crates.io, npm, PyPI, GitHub releases and Hugging Face allowed at runtime, or must the image be fully pre-baked with update checks and downloads off?
5. **GitHub.** May a sled run `gh` and `git fetch/push` (supervisor ticks, managed routes, tagent push)? If so, with which token and which hosts?
6. **Chat connectors.** Are Slack, Telegram, Google Chat and Gmail on or off on a sled? Slack Socket Mode cannot use the proxy as written.
7. **Telemetry.** No telemetry exporter exists in the workspace (no opentelemetry, sentry or posthog dependency). Claude Code's own telemetry is not configured by tm (no `DISABLE_TELEMETRY` or `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` set anywhere in `crates/`). Should a sled set those?
8. **`tagent --api`.** Remove it on sled, or move it behind the console over UDS?

## Method and coverage

- Read the worktree at `be1ed52f98` plus `docker/cloudtest/`. origin/main has since moved to `ac3d818081`, 7 commits ahead. In `crates/`, those commits change `trusty-agents/src/agents/extends/`, bump versions in three `Cargo.toml` files and add one test bench. None of them adds a listener or an outbound call (`git diff be1ed52f98 origin/main -- crates`).
- Listeners: rg for `TcpListener::bind`, `UdpSocket::bind`, `axum::serve(`, `UnixListener::bind`, `from_std`, `bind_hardened`, `bind_singleton_hardened`, `warp`, `tiny_http`, `Server::bind` across all 29 crates. Test files and inline `#[cfg(test)] mod` blocks were excluded with a script. All 7 production TCP/UDP hits were read. 13 UDS bind sites were identified; 3 (tm, search, console) were read in full, the rest by bind line only.
- Outbound: rg for hard-coded hosts (`https://...`), `reqwest`, `connect_async`, `hf_hub`, `TextEmbedding::try_new`, and `Command::new` of curl, wget, git, gh, ssh, uv, npm, cargo, docker, tailscale, claude, pip and brew. Distinct external hosts: about 45; the main ones are mapped above. `git` has 171 `Command::new` sites; only the network verbs (fetch, clone, push, pull, ls-remote) were mapped, giving 7 sites. trusty-secrets' Vercel and 1Password stores were not traced (sampled 0 of 2).
- Proxy behaviour is inferred from each client library. It was not run, except that cloudtest rung 2 shows Claude Code working through squid (`docker/cloudtest/README.md:89-91`).
- No cargo build or test was run (Mac at HOLD). No code, config or issue was changed.
