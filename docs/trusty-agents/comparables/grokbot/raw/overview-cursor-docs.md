# Grok Bot for Teams and Enterprise — Architecture excerpt (cursor.com/docs)

> Source: https://cursor.com/docs/grok-bot/teams  
> Fetched: 2026-09-05  
> Type: official Cursor docs (admin / architecture)

---

# Grok Bot for Teams and Enterprise

Grok Bot gives each person on your team standing Bots for everyday work: research, operations, documents, browsing, and automations.

## Availability

| Plan | Access |
| --- | --- |
| Individuals | Included with every paid Cursor plan, or through an individual SuperGrok link |
| Teams | Included; every member has access, and usage follows the seat's allowance |
| Enterprise | Contact your account team to enable Grok Bot for your organization |

## Architecture — How Grok Bot is built

Grok Bot is a computer-use agent that operates applications, browsers, and development environments. It runs in Cursor's cloud, and each user's work executes on a dedicated cloud computer. The desktop and mobile apps are thin clients for chat, review, and approvals.

The security model rests on four principles:

1. **Per-user isolation.** Each user's work runs in a dedicated Firecracker microVM, a micro virtual machine with hardware-level separation from other users.
2. **No access by default.** A Bot can use only the accounts and plugins the user or team grants it.
3. **Human approval gates.** Sensitive actions require user approval, evaluated by an independent review model called Auto Review.
4. **Administrative control.** Team admins can set Team Rules, Cloud Agent delegation, and template sharing. Network Controls, Team Setup, Action Recording, and the organization-wide enable switch are Enterprise only.

### Pieces

1. **Local machine.** Chat, review, and approvals happen on the member's device. Work runs in the hosted computer. Optional local execution requires per-command approval by default and can be turned off.
2. **Environment.** One persistent Firecracker microVM per user. Every Bot that user runs shares that computer.
3. **The Bot.** Shell, browser, and computer use inside the hosted computer. A Bot has no access by default and acts only with accounts the member signs it into. It hands login, 2FA, and payment steps to the member.
4. **Plugins.** Team MCP policy applies. OAuth tokens stay on Cursor's connector backend; Bots invoke tools without receiving them.
5. **Cloud Agents.** Grok Bot can delegate coding tasks to separate computers under existing Cloud Agent controls. Admins can disable spawning.
6. **Models and data.** Cursor manages model selection. With Privacy Mode enabled, customer data is not used for training.

### How users are isolated

Each user gets a dedicated computer with hardware-level separation. Within one user, all Bots share one computer; Bots isolate personalities and workspaces, not compute. When a workload needs its own computer and credential set, give it its own Cursor user.

## Admin controls (summary)

**Enterprise only:** Enable Grok Bot switch, Network Controls, Team Setup, Action Recording, Computer management, Audit logs, OpenTelemetry Export, SCIM, MCP allowlist.

**Teams + Enterprise:** Cloud Agents toggle, Public template sharing, Team Rules, Connector policy (marketplace), Auto Review team instructions.

## Recommended configuration (security-sensitive)

Admins: Network Controls; audit connectors; keep Auto Review on; set local execution policy; disable Cloud Agent spawning if unused; keep public template sharing off; add team block instructions for production/email/payments/legal terms; gate SSO to managed devices.

Members: never paste credentials into chat; prefer Allow once; sign into scoped accounts; start read-only/draft; review plugins and routines.
