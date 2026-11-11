# 0077. Cloud sled network boundary: one inbound surface, one outbound proxy, a token on every console route

- **Status:** Proposed
- **Date:** 2026-11-11
- **Scope:** Cloud sled container image (`docker/`), `trusty-console`, the egress proxy. A rule here binds a sled only; Mac and EVO hosts keep today's behaviour.
- **Reversibility Cost:** Medium — the image, the proxy config and the console token check are localized, but every sled client and every allowlisted host depends on the boundary once sleds run.
- **Decision Drivers:** owner ruling Bob D1b (2026-11-11 21:23Z, "Accept all eight as recommended"); the network surface audit; a sled that holds model credentials and a GitHub token; the console accepts any caller that sends a matching `Host` header; no ADR covers outbound traffic
- **Supersedes / Superseded by:** — (extends [ADR-0031](0031-uds-for-inter-crate-transport-http-for-external.md) and [ADR-0032](0032-no-service-owns-http-console-is-the-only-http-surface.md); both stand)
- **Spec:** [DOC-90](../specs/DOC-90-cloud-sled-network-boundary.md) §3, §4, §6, §11
- **Issue:** [#11054](https://github.com/bobmatnyc/trusty-tools/issues/11054)

## Context

A sled is one cloud container that runs the headless trusty stack. It holds
model credentials and a GitHub token, so what it can send and receive matters.

Inbound today (DOC-90 §2): trusty-console listens on `127.0.0.1:7788` with no
token and no TLS. Loopback trusts any caller whose `Host` header matches. In a
shared pod namespace, a sidecar can reach every console route. `tagent --api`
also opens its own HTTP port.

Outbound today (DOC-90 §2): about 60 call sites in 14 crates reach the
internet. None goes through the console. No ADR governs outbound traffic.

ADR-0031 and ADR-0032 already say the same-host transport is UDS and that
trusty-console is the only HTTP surface. They do not cover a container, a
token, or outbound traffic.

## Decision

1. **One inbound surface.** On a cloud sled, trusty-console is the only inbound
   surface. It stays on loopback (`127.0.0.1:7788`). Remote access is
   `tailscale serve` only. No `--tailscale`, no non-loopback `--http`, no
   Funnel, and no published container port.
2. **One outbound surface.** One allowlisted network egress proxy is the only
   outbound surface. The app network has no route out and resolves no external
   name. The proxy allows `CONNECT` to port 443 on named hosts only. It denies
   plain HTTP, other ports, and raw IP targets. Adding a host needs a named
   owner ruling, cited in the proxy config.
3. **A token on every console TCP route.** The console requires a token on
   every route of its TCP listener, with no exempt route. The token is
   provisioned as a read-only file, mounted `0400` or `0600`. It is never an
   environment value, a build arg, a command-line argument or an image layer.
   It is never logged. The console refuses to start if the file is missing,
   too short, or readable by group or other. The console's UDS stays exempt;
   `SO_PEERCRED` already gates it.

The normative rules, the host table and the verification checks are in
DOC-90. This ADR records the decision; the spec owns the detail.

## Consequences

**Easier.** A sled has two surfaces to audit. A listener scan and the proxy
deny log show whether the boundary holds. A client that ignores
`HTTPS_PROXY` fails closed because the app has no other route.

**Harder.**

- Every in-sled TCP caller of the console must send the token or move to UDS.
- A new outbound host needs a ruling and a proxy config change.
- The allowlist carries the Tailscale control hosts and the DERP host pattern
  (D20). Tailnet traffic is relay-only, so it is slower than a direct path.
- Token rotation means replacing the file and restarting the console.

**Neutral.** Hosts other than sleds are unchanged. The token is off unless
`TRUSTY_CONSOLE_TOKEN_FILE` is set.

**Alternatives considered.**

- **Route outbound traffic through the console.** Rejected. It needs new proxy
  code in the console and adds no control a network-layer proxy lacks.
- **Allowlist by IP address.** Rejected. The allowed hosts sit behind a CDN,
  so their addresses change and are shared with other sites. A host-name
  allowlist on `CONNECT` is the usable control.

**Open questions (OQ-1 and OQ-3 resolved by owner ruling D20, 2026-11-11).**

- **OQ-1 (resolved by D20).** `tagent --bind` refuses a non-loopback address on every host, not only on sleds.
- **OQ-3 (resolved by D20).** The sled egress allowlist adds `controlplane.tailscale.com`, `login.tailscale.com` and the `derpN*.tailscale.com` relay pattern, so `tailscale serve` works from inside the sled; client logging is off.

OQ-2 (Claude Code token refresh hosts) and OQ-4 (`HF_HUB_OFFLINE`) stay in
DOC-90 §11 and do not block this decision.

**Out of scope.** Tailscale Funnel and public webhook exposure. Cloud ingress
(ALB, OIDC). Any model provider other than Anthropic.

## Related Decisions

Vetted against prior ADRs on 2026-11-11:

- **ADR-0031 (Transport by purpose):** Extends — same-host traffic uses UDS and external traffic uses the shared HTTP surface. This ADR applies that split to a sled and adds the token. Its Decision stands.
- **ADR-0032 (Console is the only HTTP surface):** Extends — trusty-console remains the only HTTP surface. This ADR adds the token and the outbound proxy. Its Decision stands.
- **ADR-0017 (Shared webhook ingress via console + Tailscale Funnel):** Consistent — stays Proposed. This ADR forbids Funnel on a sled and does not decide webhook exposure.
- **ADR-0018 (Loopback-only service HTTP):** Consistent — superseded by 0032; the console stays on loopback here.
