# Approvals, security, and privacy

> Source: https://docs.x.ai/grok-bot/approvals-security-and-privacy  
> Fetched: 2026-09-05

---

# Approvals, security, and privacy

Grok Bot is designed to complete work while keeping sensitive inputs and
consequential actions under your control. Use approvals, secure handoffs, and
clear Bot boundaries together.

## Set a boundary in the request

Tell the Bot which actions it can take and where it must stop:

> Reconcile the campaign data and draft a recommended budget change. Do not
> change the campaign or message the agency. Ask for approval after showing the
> current value, proposed value, and expected impact.

Prefer explicit boundaries for:

- Sending messages or invitations
- Publishing content
- Purchases and financial transfers
- Deleting or overwriting data
- Changing permissions
- Production changes
- Accepting legal terms

An approval controls the proposed action. It does not reverse work already
completed.

## Review an action

When an action needs approval, the conversation shows the proposed operation
and its inputs. Review the target, scope, and values before approving.

- On desktop: **Allow once**, **Always allow**, **Deny**
- On iPhone and Android: **Approve once** and **Deny**

Do not approve an action whose target or effect you cannot identify.

## Configure Auto Review

When Auto Review enforcement is available, Grok Bot evaluates tool calls and
computer actions before they run. Open Settings → General → Auto-review to
add rules.

- **Require Approval** rules always stop matching actions.
- **Always Allow** rules let matching actions proceed only when the automated
  review does not identify another reason to stop.
- If both kinds of rule match, Require Approval wins.

Write narrow rules. Avoid broad rules such as "allow everything in the browser."
Personal Auto-review rules are stored on the current desktop and synced to its
Grok Bot computer.

## Enter passwords and verification codes yourself

For passwords, passkeys, two-factor codes, CAPTCHAs, and payment confirmations,
the Bot should hand you control of the computer (Agent Computer → Take control).

Do not send a password or one-time code in ordinary chat. Use the secure secret
request when presented; the value is masked, excluded from the transcript, and
not shown to the model.

## Control access to your local computer

Settings → General → Agent → Execution on Local Computer:

- Always require approval (default: Ask every time)
- Always allowed
- Never allowed

These settings do not prevent the Bot from using its cloud computer.

## Understand the shared-computer boundary

All of your Bots share one cloud computer assigned to your user account. Files,
browser sessions, and command line credentials on that computer are available
across your Bot roster.

- Do not use separate Bots as a security boundary.
- Sign out of a service when it should no longer be available.
- Remove sensitive temporary files after the work is complete.
- Delete a connector or revoke its authorization when access is no longer needed.

## Sharing a Bot is not a security boundary

A public share link lets others copy the Bot's configuration. It does not share
your computer or logins. Do not put secrets, customer data, or internal URLs in
a Bot you share.

## Cursor account and data settings

- Grok Bot uses Cursor authentication and account data settings.
- Grok Bot **requires data storage** and does **not support Legacy Privacy Mode**.
- Training opt-out follows applicable Cursor account and privacy settings.

## Remove access and working data

1. Pause or delete related routines.
2. Sign out of websites on the shared computer.
3. Uninstall connectors and revoke authorization in the source service.
4. Remove sensitive project files from `/workspace`.
5. Hide or delete Bots that should no longer appear.
6. Use account settings if you need to delete the Cursor account.

Deleting a Bot does not remove shared-computer files or browser sessions.

## Use a least-privilege setup

- Connect only the tools a workflow needs.
- Use scoped service accounts where supported.
- Start with read-only tasks and draft outputs.
- Keep sending, publishing, purchasing, deletion, and production changes behind approval.
- Review installed connectors and active routines regularly.
- Pause a routine when its source system or expected workflow changes.
- Preserve source links and an action log for important decisions.
