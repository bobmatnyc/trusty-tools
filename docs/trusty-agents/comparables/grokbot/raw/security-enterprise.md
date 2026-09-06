# Grok Bot security (Enterprise / reviewer docs)

> Source: https://cursor.com/docs/grok-bot/security  
> Cross-checked: https://cursor.com/docs/grok-bot/teams  
> Fetched: 2026-09-05

---

# Grok Bot security

Use these controls and deployment details to decide whether Grok Bot is allowed in, and to limit what Bots can access, change, and retain.

**Enterprise only on the Grok Bot dashboard:** organization-wide enable switch, Network Controls, Team Setup, Action Recording, and computer management for organization admins. Audit logs, OpenTelemetry Export, the MCP allowlist, and SCIM are also Enterprise only. Self-serve Teams do not see those settings.

## Network policy

Network Controls is Enterprise only. Modes:

| Mode | Effect |
| --- | --- |
| No policy | Allow all (default for teams without a policy) |
| Allow all network access | Explicitly allow all destinations |
| Defaults plus team allowlist | Cursor's default destinations plus your list |
| Team allowlist only | Only your list, plus destinations a computer needs to function |

- Destinations cover web domains and IP ranges with ports; no cap on entries.
- Directory groups (Enterprise) can set their own network policy.
- Policy applied when a computer is created or recreated; recreate/restart to pick up changes.
- Dedicated DLP hooks are not available.
- Blocking a plugin doesn't block that service's website — connector policy and network policy are separate layers.

## Static egress IPs

Hosted computers reach the internet through **shared static egress IP addresses**. Ranges are shared across Grok Bot customers; dedicated per-customer IPs are not available. Treat ranges as identifying Grok Bot traffic rather than your team alone. Current ranges from account team.

Team Setup (Enterprise) can install networking clients (e.g. Tailscale / Cloudflare Tunnel) to reach private services — separate from shared egress.

## Approvals and Auto Review

Strongest boundary is in the request itself. When an action needs approval: proposed operation + inputs; Allow once / Always allow / Deny.

**Auto Review** is an independent review model that evaluates risky Bot actions before they run, covering:

- shell commands
- plugin calls
- computer use
- automation writes (routines and event triggers)
- delegation such as Cloud Agent and subagent launches

It can let an action proceed, require approval, or deny it.

- Enforcement is enabled by Cursor and currently active for all users. Each member's own Auto Review setting remains the off switch; an organization-level lock is not available.
- Team instructions (allow/block) shape it for everyone (Team Settings → Security and Automation).
- Members can add personal Require Approval / Always Allow rules.
- It doesn't review every side effect (e.g. memory writes, most settings changes).
- Complement with per-action approvals, network policy, and per-user isolation.

## Identity and sign-ins

- Members sign in with Cursor account; existing Cursor SSO applies (SAML 2.0: Okta, Entra, Google Workspace, OneLogin). SSO can be required (blocks password login).
- SCIM 2.0 provisioning/deprovisioning: Enterprise only.
- Inside the hosted computer, members sign into apps via their own IdP in the browser.
- A Bot has **no identity or credentials of its own** — acts as the signed-in member. Team-managed connectors may use team/service-account credentials.
- Connector tokens stay on Cursor's backend; never stored on the computer.
- For login/2FA/payment, Bot hands computer to member; secure secret request masks values.
- Org admin can terminate a member's computer (Enterprise). Durable disk kept; next session starts fresh computer.
- Deleting a Bot doesn't remove computer files or browser sessions.

## Logging and audit

- **Audit logs** (Enterprise): admin/security/auth + Grok Bot control-plane (Bot creation, access changes, Team Setup manifests, MCP auth, Slack account links, routines). Filter by application; SIEM stream.
- **Action Recording** (Enterprise): off by default; records Bot actions including scrubbed shell commands; 90-day retention; tagged `cursor.surface=grok_bot` via OTel Export. Does not appear on Audit Log page. Privacy Mode (Legacy) forces recording off.

## Endpoint tooling

No built-in customer-facing telemetry/EDR feed. Computers are Cursor-operated; Cursor monitors for health/abuse excluding customer data. Team Setup can install customer tooling.

## Data retention and deletion

- Durable disk keeps files, browser sessions across sessions.
- Idle hibernation ≠ deletion.
- Image updates preserve files.
- Member reset keeps synced durable data; recent unsynced work can be lost.
- Deletion follows DPA: within 30 days of written direction after service ends.
- Daily encrypted backups of production control plane.
- No per-org retention policy or customer-managed PITR of individual computers.

## Data residency

Grok Bot computers run in the **United States** today. That is not the same as Cursor's US-only data residency program (does not apply to Grok Bot by default). Written residency commitment: contact account team.

## Models and data

- Cursor manages model selection; **no customer-facing model picker**; serving mix can change.
- Team model allowlist is Enterprise only; enforcement not guaranteed.
- Privacy Mode applies; with Privacy Mode enabled, customer data not used for training.
- Zero Data Retention follows Cursor's existing provider agreements. Providers may run abuse/safety classifiers; flagged data may be stored for investigation.

## Local execution

Separate from hosted computer. Per-command approval default. Policy: ask every time / always / never. Recommend Never unless specific reason. Can be disabled entirely; team-level ceiling via settings (no dashboard control for ceiling today).

## Hosting

**Cursor-hosted cloud computers only.** No on-prem, no deployment inside customer perimeter, no BYO image. No VPN/tunnel/private link operated by Cursor into customer network for Grok Bot. Supported model: shared static egress + destination allowlist. Team Setup for private nets.

## Prompt injection

Outside content (web pages, plugin results, command output) can try to steer the Bot. Defenses: Auto Review; network policy; per-action approvals; per-user isolation; outside content marked as untrusted data when presented to the model. Controls reduce but don't eliminate risk.

## Certifications

Anysphere (Cursor) holds ISO/IEC 27001 and ISO/IEC 42001 (Schellman). Grok Bot included in current ISO scope. Certificates at trust.cursor.com.
