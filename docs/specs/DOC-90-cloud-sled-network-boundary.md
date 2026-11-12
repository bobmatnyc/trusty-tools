# DOC-90 — Cloud sled network boundary

**Status:** Draft
**Subsystem:** sled container image (`docker/`), `trusty-console` (inbound, token), `trusty-agents` (`tagent --api`), `trusty-common` / `trusty-agents` update checks, egress proxy
**Owner:** owner
**Last-updated:** 2026-11-11
**Spec ID:** `SPEC-SLEDNET-01~draft` … `SPEC-SLEDNET-11~draft` (DOC-90)
**DOC-N claim:** `DOC-90`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). The catalog's next-free note on `origin/main` at 2026-11-11 (`7497139ef9`) named `DOC-90`; no file or open PR claims it.

Owner ruling (Bob, D1b, 2026-11-11 21:23Z): "Accept all eight as recommended." The eight are the recommendations of the network surface audit, an owner brief that is not published in this repo; its finding IDs are cited below as "audit".

A **sled** is one cloud-hosted container (or pod) that runs the headless trusty stack. A **host** is the machine or node it runs on. The reference build is `docker/cloudtest/`, on `main` since `fb3d642382`.

## 1. Scope and ruling {#SPEC-SLEDNET-01~draft}

**ID:** SPEC-SLEDNET-01~draft

The sled has exactly two network surfaces:

- **Inbound:** trusty-console. Nothing else in the sled accepts a connection from outside the sled.
- **Outbound:** one allowlisted network egress proxy. Nothing in the sled reaches the internet except through it.

The ruling also accepts audit recommendations 2 to 8. §3 to §8 state them as testable rules. §9 states how a sled is verified. A rule here binds a sled only; Mac and EVO hosts keep today's behaviour unless a section says otherwise. AG-2 says otherwise (owner ruling D20).

## 2. Current state {#SPEC-SLEDNET-02~draft}

**ID:** SPEC-SLEDNET-02~draft

From the audit, read at `be1ed52f98`. Spot-checked on `main` at `fb3d642382` for this spec; the cited lines still hold.

| Area | Current state | Citation |
|---|---|---|
| Console inbound | Loopback `127.0.0.1:7788`; no token, no TLS; loopback trusts any caller whose `Host` matches, MagicDNS name included | audit L1; `crates/trusty-console/src/lib.rs:38,184`, `src/tailnet_peer/loopback_origin.rs:1-15` |
| Console tailnet | `--tailscale` adds the tailnet IP; same-login `tailscale whois` gate | audit L2; `src/bind.rs:128-146`, `src/tailnet_peer/mod.rs:1-24` |
| Funnel | `BindMode` has Local, Tailscale, Explicit only; ADR-0017 is Proposed | audit; `src/bind.rs:29-36` |
| `tagent --api` | Own HTTP port, default `127.0.0.1:8080`; non-loopback `--bind` allowed with a token; console proxies `/api/agents/*` to it over HTTP | audit L3; `crates/trusty-agents/src/runtime/mode_dispatch.rs:205`, `src/api/server/routes.rs:610-617,766-772` |
| Other TCP | Desktop and OAuth loopback listeners, UDP LAN probe | audit L4-L7 |
| UDS | 13 listeners, `bind_hardened` (0600, `SO_PEERCRED`) | audit; `crates/trusty-common/src/lib.rs:1396-1412` |
| Outbound | 23 groups, about 60 sites, in 14 crates; none through the console; all but Slack Socket Mode honour `HTTPS_PROXY` (inferred) | audit O1-O23 |
| Update checks | memory and search check at daemon start; `TRUSTY_NO_UPDATE_CHECK` turns them off | audit O18; `crates/trusty-common/src/update/mod.rs:29,335-340` |
| tagent update check | Calls `api.github.com` at TUI start and does **not** read `TRUSTY_NO_UPDATE_CHECK` | `crates/trusty-agents/src/update/mod.rs:13,55`, `src/repl/tui/run.rs:104` (found for this spec) |
| hf-hub offline | `HF_HUB_OFFLINE` is not read by hf-hub 0.3.2, hf-hub 0.5.0 or fastembed 5 sources in the local cargo registry | found for this spec; corrects the audit's "inferred to be honoured" (gap 7) |
| PR-cleanup tick | `TRUSTY_MPM_PR_CLEANUP_INTERVAL=0` already turns it off | `crates/trusty-mpm/src/supervisor/config.rs:79-86` |
| Listener check | `tm doctor` row `tcp_listeners` reads `/proc` on Linux and grades against `tcp_listener_allowlist.tsv` | `crates/trusty-mpm/src/daemon/doctor_tcp_listeners.rs:1-19` |
| Egress pattern | App on an `--internal` network; squid allows `CONNECT :443` to three Anthropic hosts | `docker/cloudtest/proxy/squid.conf:13-24`, `docker/cloudtest/Dockerfile:84`, `docker/cloudtest/README.md:56-123` |

## 3. Inbound rule {#SPEC-SLEDNET-03~draft}

**ID:** SPEC-SLEDNET-03~draft

- **IN-1.** In the sled network namespace, the only trusty-* TCP listener is trusty-console on `127.0.0.1:7788`. No trusty-* process listens on a non-loopback address or on UDP.
- **IN-2.** Remote access is Tailscale only: `tailscale serve` terminates TLS on the tailnet name and forwards to `http://127.0.0.1:7788`. The console is not started with `--tailscale` or a non-loopback `--http`.
- **IN-3.** No Tailscale Funnel runs on a sled. `tailscale funnel status` reports no funnel.
- **IN-4.** Every request on the console's TCP listener carries a valid console token (§4). The console's own UDS is exempt; `SO_PEERCRED` already gates it.
- **IN-5.** The listeners `tailscaled` itself owns (its WireGuard UDP port and the `tailscale serve` front on the tailnet IP) are the only non-trusty listeners allowed. They are named in the §9 inventory.
- **IN-6.** The orchestrator publishes no port of the sled (`docker run` with no `-p`; a Kubernetes Service, if any, targets nothing in the sled).

## 4. Console token {#SPEC-SLEDNET-04~draft}

**ID:** SPEC-SLEDNET-04~draft

**What it protects.** Every route on the console's TCP listener: the UI, `/api/{service}/{*path}` (including `/api/agents/*`), `/tools/*`, `/api/secrets`, the event streams and `/api/webhooks/{source}`. No route is exempt. It closes audit gap 2: a sidecar in a shared pod namespace, or a caller that sends the right `Host`, no longer reaches every route.

**Requirements.**

- **TOK-1. Activation.** `TRUSTY_CONSOLE_TOKEN_FILE=<path>` turns the token on. The sled image sets it. Unset, the console behaves as today (Mac, EVO).
- **TOK-2. Provisioning.** The operator generates the token outside the sled (at least 32 random bytes, hex) and keeps it in their secret store. The orchestrator mounts it read-only at the path, as a file: a Docker secret or a Kubernetes Secret volume, mode `0400` or `0600`, owned by the daemon uid. The token never arrives as an environment value, a build arg, a command-line argument or an image layer.
- **TOK-3. Fail closed at start.** With the variable set, the console exits non-zero before it binds if the file is missing, empty, shorter than 32 bytes, or readable by group or other. The error names the path and the reason, never any byte of the contents.
- **TOK-4. Request check.** A request passes with `Authorization: Bearer <token>`, compared in constant time. The existing Host/Origin checks (`loopback_origin.rs`) still apply first.
- **TOK-5. Browser path.** `GET /login` serves a form; `POST /login` with the right token sets an `HttpOnly; Secure; SameSite=Strict` cookie holding a server-minted random session id, valid 12 h. The cookie never holds the token. Sessions live in memory and end on restart.
- **TOK-6. Failure mode.** A missing or wrong token gets `401` with `WWW-Authenticate: Bearer` and a fixed body. The response does not say which check failed. A browser request without a session redirects to `/login`.
- **TOK-7. Never logged.** The token and the session id appear in no log line, tracing field, error message, panic message, `tm doctor` output, `/health` body or metrics label. Request logging redacts the `Authorization` and `Cookie` headers. `tm doctor` reports the token as present or absent, with mode and length only.
- **TOK-8. In-sled TCP clients.** Each trusty-* client that calls the console over TCP either moves to the console UDS or reads the same file and sends the header. Known TCP callers: `crates/trusty-mpm/src/core/discovery.rs`, `src/bin/tm/commands/statusline/mod.rs`, `src/bin/tm/commands/socket_dispatch.rs`, `crates/trusty-installer/src/commands/probe_http.rs`, `src/commands/ui.rs`.
- **TOK-9. Rotation.** Replace the file and restart the console. Old sessions end with the restart.

## 5. `tagent --api` rule {#SPEC-SLEDNET-05~draft}

**ID:** SPEC-SLEDNET-05~draft

- **AG-1. Off on sleds now.** The sled image sets `TAGENT_NO_API=1` (named after the `TAGENT_NO_PROVISION` precedent, #10940). With it set, `tagent --api` and `tagent --serve` exit non-zero before binding, with a message that names [#6433](https://github.com/bobmatnyc/trusty-tools/issues/6433). The sled entrypoint never starts either mode.
- **AG-2. Non-loopback `--bind` refused on every host.** `serve_with_config` refuses a non-loopback bind even with a token; today it accepts one (`routes.rs:610-617`). The rule holds on every host, Mac and EVO included, not only sleds. Token-gated LAN use of `tagent --api` ends. The UDP LAN-address probe (audit L4) only runs on a non-loopback bind, so it goes with this rule. Owner ruling D20 (2026-11-11) settled OQ-1.
- **AG-3. End state.** [#6433](https://github.com/bobmatnyc/trusty-tools/issues/6433) moves the tagent API to UDS. The console proxies `/api/agents/*` over UDS, the `http_addr` discovery file goes, and the `trusty-agents` row in `tcp_listener_allowlist.tsv` is removed. That closes the `/api/agents` bypass (audit, "What the console needs" item 7). AG-1 is then lifted for the UDS mode only.

## 6. Outbound rule {#SPEC-SLEDNET-06~draft}

**ID:** SPEC-SLEDNET-06~draft

- **OUT-1. No route out.** The sled's application network namespace has no route to the internet and resolves no external DNS name. Docker: an `--internal` network. Kubernetes: a NetworkPolicy that denies all egress except to the proxy. The proxy is the only peer outside the sled that the app can reach.
- **OUT-2. One proxy.** One squid proxy, outside the app namespace, accepts `CONNECT` to port 443 only, from the app subnet only. It denies plain HTTP, every other port, and any `CONNECT` whose target is a raw IPv4 or IPv6 address. It caches nothing and logs every decision.
- **OUT-3. Allowlist by host.** Only the hosts in the table below are allowed. Squid sees the host name only, not the URL path, so a host on the list is allowed for every path.
- **OUT-4. Two profiles.** `steady` holds the steady-state rows. `login` adds the login rows. The proxy runs `steady` except while the operator runs a Claude Code `/login`.
- **OUT-5. Changes need a ruling.** A host is added only by a named owner ruling cited in the config comment. A new model provider needs its own ruling (ruling 3).
- **OUT-6. Proxy env.** Every process in the sled inherits `HTTPS_PROXY`, `https_proxy`, `NO_PROXY`, `no_proxy` (§7). A client that ignores them fails closed because of OUT-1.

| Host | Profile | Used by | Reason |
|---|---|---|---|
| `api.anthropic.com` | steady | Claude Code sessions (audit O7); trusty inference on the Anthropic provider (O1, O2, O3) | Model API, the one provider allowed (ruling 3) |
| `github.com` | steady | `git fetch`, `clone`, `push` over HTTPS from managed routes and tagent (O17) | Source access for the sled's repos (ruling 5) |
| `api.github.com` | steady | `gh` and GitHub REST from managed routes, ticket tools (O13, O16) | PR and issue calls for the sled's repos (ruling 5) |
| `controlplane.tailscale.com` | steady | `tailscaled` coordination: auth, keys, netmap (OQ-3) | `tailscale serve` needs a tailnet login (ruling D20) |
| `login.tailscale.com` | steady | `tailscaled` login and coordination fallback | Same (ruling D20) |
| DERP relays, matched by `dstdom_regex` `^derp[0-9]+[a-z]*(-all)?\.tailscale\.com$` | steady | `tailscaled` relayed traffic | Only path to peers through the proxy (ruling D20); see the Tailscale paragraph below |
| `platform.claude.com` | login | Claude Code `/login` | OAuth login (ruling 3); see OQ-2 on token refresh |
| `claude.ai` | login | Claude Code `/login` | OAuth login (ruling 3) |

Not on the list, so denied: crates.io, static.crates.io, index.crates.io, registry.npmjs.org, pypi.org, files.pythonhosted.org, huggingface.co, downloads.claude.ai, objects.githubusercontent.com, every other model provider, Slack, Telegram, Google, Granola, Linear, Jira, Confluence, open-meteo.com. `log.tailscale.com`, `log.tailscale.io` and `console.tailscale.com` are also denied.

**Tailscale hosts (ruling D20, OQ-3).** `tailscaled` needs outbound TCP 443 to `controlplane.tailscale.com` and `login.tailscale.com`, and to the DERP relays ([Tailscale firewall ports](https://tailscale.com/kb/1082/firewall-ports)). Closed list: the two control hosts. DERP cannot be a closed list. The DERP map holds 95 hosts in 28 regions, all named `derp<region><letter>.tailscale.com`, and Tailscale says the count may grow ([DERP map](https://controlplane.tailscale.com/derpmap/default)). The proxy therefore matches that pattern only. It does not allow `.tailscale.com` as a whole, so `log`, `console` and the website stay denied. Squid rule: `acl derp dstdom_regex -i ^derp[0-9]+[a-z]*(-all)?\.tailscale\.com$`, plus `dstdomain controlplane.tailscale.com login.tailscale.com`. The control plane prefers TCP 80 and falls back to 443; the proxy accepts 443 only, so the fallback is taken.

- **Relay-only.** An HTTPS `CONNECT` proxy carries TCP only. Direct WireGuard paths (UDP, source port 41641) and STUN (UDP 3478, sent to DERP servers) cannot use it, and OUT-1 leaves the app no UDP route. Every tailnet connection to a sled is relayed over DERP, with higher latency than a direct path. `tailscale serve` to the console still works.
- **Logging decision.** The sled image sets `TS_NO_LOGS_NO_SUPPORT=true` for `tailscaled` ([logging docs](https://tailscale.com/kb/1011/log-mesh-traffic)). The log host is then not needed and stays denied. Cost: Tailscale may not give support for a sled's node ([tailscaled flags](https://tailscale.com/kb/1278/tailscaled)). A sled is a no-route namespace, so sending client logs to a third party is the wrong default.
- **Unverified.** Tailscale's KB does not say that `tailscaled` honours `HTTPS_PROXY`. Rung 2 must start `tailscaled` with the §7 proxy env, run `tailscale serve`, and read `access.log` for every `CONNECT` host. A host seen there and not allowed is a finding for a new ruling (OUT-5).

**GitHub credential.** One fine-grained token per sled, scoped to that sled's repos, mounted read-only like the console token and never set as an image env value. Only managed routes push (ruling 5).

## 7. Sled image environment {#SPEC-SLEDNET-07~draft}

**ID:** SPEC-SLEDNET-07~draft

The image sets each variable below in its `ENV` layer. None holds a secret.

| Variable | Value | Effect | Source |
|---|---|---|---|
| `HTTPS_PROXY`, `https_proxy` | `http://<proxy>:3128` | Clients send TLS through the proxy | OUT-6 |
| `NO_PROXY`, `no_proxy` | `localhost,127.0.0.1,::1` | Loopback stays direct | OUT-6 |
| `TRUSTY_NO_UPDATE_CHECK` | `1` | No update check at daemon or CLI start | ruling 4 |
| `TRUSTY_UV_FETCH` | `0` | No uv, Python or wheel download (`crates/trusty-embedderd-py/src/uv_fetch.rs:240-246`) | ruling 4 |
| `HF_HUB_OFFLINE` | `1` | Set per ruling 4. Not read by the pinned hf-hub (§2); the baked model cache and OUT-3 carry the guarantee | ruling 4 |
| `DISABLE_AUTOUPDATER` | `1` | Claude Code does not self-update | ruling 4 |
| `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` | `1` | Claude Code sends no non-essential traffic | ruling 7 |
| `DISABLE_TELEMETRY` | `1` | Claude Code telemetry off | ruling 7 |
| `DISABLE_ERROR_REPORTING` | `1` | Claude Code error reporting off | ruling 7 |
| `ENABLE_CLAUDEAI_MCP_SERVERS` | `false` | No claude.ai-hosted connectors in Claude Code | ruling 6 |
| `TRUSTY_MPM_PR_CLEANUP_INTERVAL` | `0` | Supervisor PR-cleanup tick off | ruling 5 |
| `TAGENT_NO_API` | `1` | `tagent --api` / `--serve` refused | AG-1 |
| `TRUSTY_CONSOLE_TOKEN_FILE` | `/run/secrets/trusty-console-token` | Console token on (§4) | ruling 2 |
| `FASTEMBED_CACHE_DIR` | `/models/fastembed` | Points at the baked or `:ro`-mounted model cache | ruling 4 |

**Pre-bake.** The image holds release binaries verified by sha256 twice (download script and build stage), the Claude Code binary pinned by version and sha256, and the embedding models baked in or mounted `:ro`. The build may reach the network; the running sled may not. This is the `docker/cloudtest/Dockerfile` pattern.

## 8. Per-feature defaults on a sled {#SPEC-SLEDNET-08~draft}

**ID:** SPEC-SLEDNET-08~draft

| Feature | Default | Mechanism | Re-enable condition |
|---|---|---|---|
| Console on loopback | On, token required | §3, §4 | — |
| Console `--tailscale` / non-loopback `--http` | Off | IN-2 | — |
| Tailscale serve in front of the console | On | IN-2 | Hosts allowed by D20; relay-only (§6) |
| Tailscale Funnel; cloud ingress (TLS + OIDC) | Off | IN-3 | A sled that needs it, a named ruling, and ADR-0017 built with per-route exposure |
| `tagent --api` / `--serve` | Off | AG-1 | #6433 UDS mode |
| Update checks (memory, search, analyze, tagent) | Off | `TRUSTY_NO_UPDATE_CHECK=1`; tagent needs code (§2) | — |
| `upgrade`, `tctl up`, `tm reinstall`, Claude Code self-update | Off | `DISABLE_AUTOUPDATER=1`; registries not allowlisted | Rebuild the image instead |
| Embedding model download | Off | Baked or `:ro` cache; huggingface.co denied | — |
| uv / Python embedder download | Off | `TRUSTY_UV_FETCH=0` | — |
| Claude Code sessions | On | Through the proxy, `steady` profile | — |
| Claude Code WebFetch to arbitrary hosts | Off in effect | Denied by OUT-3 | Per-host ruling |
| Model provider other than Anthropic (OpenRouter, Fireworks, Together, AtlasCloud, OpenAI) | Off | Not allowlisted; sled config selects Anthropic | Named ruling per provider |
| Features that default to OpenRouter: tm overseer (O2), memory dream (O4), review LLM (O5), tagent embeddings and context indexer (O3) | Off, or switched to Anthropic where the provider is selectable | Config | — |
| `git` / `gh` from managed routes | On | `github.com`, `api.github.com`; per-sled token | — |
| Supervisor PR-cleanup tick | Off | `TRUSTY_MPM_PR_CLEANUP_INTERVAL=0` | — |
| tm bug report, content release fetch (O15) | Off | Not configured; the sled token cannot reach trusty-tools | — |
| Chat connectors: tm and tagent Slack, Telegram, Google Chat (O8-O10) | Off | No channel config or token in the sled | Client honours `HTTPS_PROXY` (Slack Socket Mode does not today) and its hosts are allowlisted (ruling 6) |
| Gmail, Calendar, Google connectors, Granola, Linear, Jira, Confluence, open-meteo (O11-O14) | Off | Not configured; not allowlisted | Per-tool ruling |
| trusty-secrets Vercel and 1Password stores | Off | Not configured | Trace first; the audit did not (Method) |
| Local Ollama (O23) | Off | Not configured | Loopback only; needs a model host in the sled |

## 9. Verification {#SPEC-SLEDNET-09~draft}

**ID:** SPEC-SLEDNET-09~draft

A sled passes when V1 to V4 all pass on the running sled. Each check prints PASS or FAIL per row and exits non-zero on any FAIL.

**V1 — squid deny log.** After a standard exercise (stack start, `check.sh`, one Claude Code session, one `git fetch` and one `gh api` call on a sled repo):

- Every `TCP_TUNNEL/200` line in `access.log` names a `steady` host (or a `login` host inside a logged login window).
- A probe `CONNECT example.com:443` from the app is logged `TCP_DENIED/403`.
- A probe `CONNECT <raw IP>:443` is logged `TCP_DENIED/403`.
- No `CONNECT` to crates.io, huggingface.co, registry.npmjs.org, pypi.org or downloads.claude.ai appears at all, denied or allowed: the pre-baked image makes no such attempt.
- From the app, `curl --noproxy '*' https://example.com` fails, and `getent hosts example.com` resolves nothing (OUT-1).

**V2 — listener inventory.** Inside the sled:

- `tm doctor` row `tcp_listeners` is OK, and lists exactly one trusty-* TCP listener: trusty-console on `127.0.0.1:7788`.
- `/proc/net/tcp`, `tcp6`, `udp` and `udp6` show no LISTEN or bound socket beyond that one and the `tailscaled` sockets of IN-5.
- `tailscale funnel status` shows no funnel.

**V3 — env check.** A script compares each §7 variable against its expected value and prints the name and PASS or FAIL, never the value. It also checks that the console token file exists with mode `0400` or `0600`, and the GitHub token file likewise, by `stat` only, never by reading.

**V4 — console token.** From inside the sled: a request with no token gets `401`; a wrong token gets `401`; the right token gets `200`. Then a search of the console log, the stack logs and `tm doctor` output for the token value finds zero hits. Starting the console with a group-readable token file exits non-zero.

V1 cannot tell an update check from a ticket call on `api.github.com`: squid sees the host only. The update-check rule is verified by V3 and by the tagent unit test that the check is skipped under `TRUSTY_NO_UPDATE_CHECK`.

## 10. Out of scope {#SPEC-SLEDNET-10~draft}

**ID:** SPEC-SLEDNET-10~draft

- Cloud ingress: an ALB or EKS ingress, its TLS, and OIDC. Added only when a sled needs it (ruling 2).
- Tailscale Funnel and public webhook ingress (ADR-0017, `BindMode::Funnel`).
- Any model provider other than Anthropic, and a console model gateway.
- Console as an outbound proxy. The egress proxy is network-layer; no trusty-* code proxies outbound traffic.
- Proxy support for Slack Socket Mode websockets.
- Rate limits, request size limits and access logging on console routes (audit, "Not checked").
- Mac and EVO host listeners and egress. This spec changes neither, except AG-2, which refuses a non-loopback `tagent --bind` on every host (owner ruling D20).

## 11. ADR call and open questions {#SPEC-SLEDNET-11~draft}

**ID:** SPEC-SLEDNET-11~draft

**ADR.** A new ADR, not an amendment to ADR-0032. ADR-0032 and ADR-0031 cover inbound HTTP and inter-service transport; this spec's inbound rule is their rule applied to a sled, plus a token, so their Decisions stand unchanged. The outbound rule is a new decision with no ADR today: one network egress proxy with a host allowlist, changed only by named ruling. The new ADR records that rule and the console-token requirement, and cross-links ADR-0032 (immutability, DOC-46 §4) and ADR-0017, which stays Proposed. The decision is recorded as [ADR-0076](../adr/0076-cloud-sled-network-boundary.md).

**Open questions.**

- **OQ-1. Resolved by D20 (2026-11-11).** AG-2 refuses a non-loopback `--bind` on every host. Token-gated LAN use of `tagent --api` on the Mac ends.
- **OQ-2.** `docker/cloudtest/README.md:112-113` says Claude Code token refresh also needs `platform.claude.com` and `claude.ai`. If so, `steady` breaks at token expiry. Measure in rung 2 (run past one refresh on `steady` and read `access.log`), then either move those hosts to `steady` by ruling or keep the login window.
- **OQ-3. Resolved by D20 (2026-11-11).** The Tailscale control hosts and the DERP pattern join the `steady` allowlist (§6). Tailnet traffic is relay-only. Client logging is off. Rung 2 confirms `tailscaled` uses the proxy.
- **OQ-4.** The pinned hf-hub ignores `HF_HUB_OFFLINE`. Keep the variable as ruled, or drop it as dead config.

## References

- [ADR-0076](../adr/0076-cloud-sled-network-boundary.md) (Proposed) records the inbound, outbound and token decisions of this spec.
