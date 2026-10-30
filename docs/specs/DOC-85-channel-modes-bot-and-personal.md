# DOC-85 — Channel modes: bot / service account and personal

**Status:** Draft (design cleared 2026-10-27; no code yet)
**Subsystem:** `trusty-channels` — route policy, host ceiling, audit; `trusty-common` — credential registry; `trusty-agents` — channel adapters
**Owner:** owner
**Last-updated:** 2026-10-28
**Spec ID:** `SPEC-CHMODE-01~draft` … `SPEC-CHMODE-08~draft` (DOC-85)
**DOC-N claim:** `DOC-85`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft); `DOC-84` was the highest on `origin/main` (2026-10-27).
**Builds on:** [DOC-45](./DOC-45-credential-authority-model.md) (principal, scope, audit); [DOC-74](./DOC-74-secrets-integration.md) (storage, owner scope); [DOC-60](./DOC-60-bus-based-agent-messaging.md) §8 (channels ride the bus).

This spec generalizes the Notion ruling to every channel: Notion, Slack, Google Chat, Google Docs, Telegram and later ones. Owner instruction (Bob, 2026-10-27 12:19Z): channels have two modes, bot / service account and personal. Rulings are cited by id (Dd7, Dd8, Dd9, Dda, De6, Ddb, the 12:17Z per-use map); the rationale stays in the source plan `.trusty-mpm/plans/2026-10-27-channels-modes-plan-df.md` (untracked, local).

---

## 1. Modes {#SPEC-CHMODE-01~draft}

**ID:** SPEC-CHMODE-01~draft

A channel call runs in exactly one mode.

| Mode | Purpose | Identity that acts |
|---|---|---|
| `bot` (bot / service account) | The system acts as an agent. | The integration's own identity. |
| `personal` | The system acts as a user proxy for the owner. | The owner, through the owner's own credential. |

`bot` is the default. `personal` is never implicit (section 3).

## 2. What each mode may do {#SPEC-CHMODE-02~draft}

**ID:** SPEC-CHMODE-02~draft

"Route" is a committed route in `.trusty-channels/routes.toml` that is in effect. "Ask" is a per-call `ask_owner` approval. "Listed" means a resource (page, database, space, doc, channel) named in the route. The `personal` column applies on every channel (Dd8, Dd9, Dda).

| Action | `bot` | `personal` |
|---|---|---|
| Read content shared with the integration | Allow (route). | Allow if listed in the route and the host enables `personal` (Dd8). |
| Read other private content of the owner | Deny. Never widen sharing to make it work. | Ask per call. A host enable alone never opens it (Dd8). |
| Write or post | Allow where a route lists the target. | Ask per call; a route never replaces the Ask (Dd9). |
| Outward message (comment, mention, share, invite) | Allow when a route names the recipient and kind. | Ask per call (Dd9). |
| Delete | Deny. | Deny (Dd9). |
| Audit line | Every action. | Every action; the only mark of the proxy (Dda, section 6). |

A mode never grants more than the route and the host ceiling already allow. The table is the most a mode may do.

Separately from modes, the `tm hook --egress-guard` backstop (#8454) refuses every Slack MCP tool in a tm-launched session. Its one exception is the host `channels.slack_read_allowlist` (#10387): a listed project may call `slack_read_channel` and `slack_read_thread` on the `slack-mcp` or `claude_ai_Slack` server, and nothing else. Schema and rules: `crates/trusty-channels/src/policy/slack_read.rs`.

### 2.1 Service-account scope {#SPEC-CHMODE-02A~draft}

**ID:** SPEC-CHMODE-02A~draft

A bot / service account sees only the resources selected for it: shared per page, channel or doc. It is never granted workspace-wide access by default. A need that the shared set does not cover is a new share, decided by the owner, not a wider credential.

## 3. Per-use routes and mode selection {#SPEC-CHMODE-03~draft}

**ID:** SPEC-CHMODE-03~draft

Each route names exactly one credential ref. The use picks the ref (PD-5). A host lists the refs a route may pick (`bot_refs` for bot, one `personal_ref`); a route outside the list is a widening and refuses the file. A route with no ref where the host lists more than one is denied. A host that lists one ref lets a route omit it.

Authority order, highest first: host ceiling, then route, then call.

1. Host: `channels.<name>.modes` lists the permitted modes. Absent means `[bot]`.
2. Route: an optional `mode` sets the default for calls through it. `personal` needs the host to permit it and list the project.
3. Call: an optional `mode` selects only a mode the route and host both permit.

Effective mode = call field, else route `mode`, else `bot`. A result outside the host `modes` is denied with reason `mode_not_allowed`. There is no fallback to the other mode. A missing field, missing file or host fault resolves to `bot` or to deny, never to `personal` (PD-1).

## 4. Credential binding {#SPEC-CHMODE-04~draft}

**ID:** SPEC-CHMODE-04~draft

| Slot | Mode | Accepts |
|---|---|---|
| `bot_ref` | `bot` | An integration or service-account credential, by registry key. |
| `personal_ref` | `personal` | A user credential, by registry key. Host file only. |

- A user credential is accepted only in `personal_ref`, on every channel, and refused in `bot_ref` (Dd7). An integration credential in `personal_ref` is refused.
- A call never reads the other mode's slot.
- Each ref resolves only from its own owner-scope secure store (DOC-74 §15.3). There is no process-env and no `.env.local` fallback. A project file cannot reach the owner scope (DOC-74 owner ruling 06).
- `bot_ref` and `personal_ref` may sit in different scopes; each resolves from its own scope only.
- Refs are names. No token value appears in any file, log or error. `secret://` values are refused without echo (PD-2).

## 5. Per-channel credentials {#SPEC-CHMODE-05~draft}

**ID:** SPEC-CHMODE-05~draft

| Channel | Bot / service-account credential | Personal credential | API doc |
|---|---|---|---|
| Slack | Bot token `xoxb-` (registry `slack`); Socket Mode app token `slack-app` is receive-only, not a `bot_ref` | User token `xoxp-` (registry `slack-user`, exists) | [tokens](https://docs.slack.dev/authentication/tokens) |
| Notion | Internal connection token, `bot.owner.type = workspace` (keys `notion-companion`, `notion-aria`, `notion-cto`) | Personal access token acting as its creator (key `notion-user`, De6) | [authorization](https://developers.notion.com/guides/get-started/authorization), [PAT guide](https://developers.notion.com/guides/get-started/personal-access-tokens) |
| Google Chat | Workspace service account (`key_file`) | OAuth user credential (key `gchat-user`, new row) | [Chat auth](https://developers.google.com/workspace/chat/authenticate-authorize) |
| Google Docs | Workspace service account, with docs shared to it | User OAuth credential (key to be named; `google-oauth` is a client secret, not a user token) | [Docs API auth](https://developers.google.com/workspace/docs/api/auth) (OAuth user flow), [service accounts](https://developers.google.com/identity/protocols/oauth2/service-account) |
| Telegram | Bot API token (registry `telegram`) | User-account (MTProto) session (key `telegram-user`, new row) | [Bot API](https://core.telegram.org/bots/api), [API](https://core.telegram.org/api) |

Gaps, as of 2026-10-27:

- `telegram-user` has no code. The repo has Bot API code only; the row names a credential kind that nothing reads.
- `gchat-user` and the Google Docs personal key have no registry row. `Channel` has no `Notion` or Google Docs variant, and the registry has no `notion-*` rows (`crates/trusty-channels/src/policy/types.rs:15`, `crates/trusty-common/src/credential_registry.rs:45`).
- Google Docs is reached through the `Gworkspace` adapter in `trusty-agents`, outside the `Channel` enum. Bringing it under the mode rules is follow-up work.
- Other channels add a row here before they accept a `personal_ref`. A channel with no user-credential type has no `personal_ref`.

## 6. Audit {#SPEC-CHMODE-06~draft}

**ID:** SPEC-CHMODE-06~draft

Every audit line and tool result carries `mode` and `actor` (integration id or owner id). A deny line carries `mode` and the reason code. A `personal` outward action also records the approval id of the Ask. Records hold no message text. The audit line is the only mark of a proxy action; no footer or marker on personal-mode content; bot-mode posts carry the section 8 footer (D58) (Dda). Where the service shows only the owner as author, only the audit line tells proxy from owner.

## 7. Worked example: Notion, and the amendment {#SPEC-CHMODE-07~draft}

**ID:** SPEC-CHMODE-07~draft

Notion credential per use (Bob ruling, 12:17Z; De6; PD-5). Refs and scopes only.

| Use | Mode | Ref | Registry key | Store scope |
|---|---|---|---|---|
| Trusty brief sharing (a brief synced to Docs or Notion) | personal | `NOTION.NOTION_PAT` | `notion-user` | `trusty/bob-duetto` |
| APEX Companion | bot | `NOTION.NOTION_CONNECTOR_APEX_COMPANION` | `notion-companion` | `trusty/duettoresearch` |
| mcp-services and all internal tooling | bot | `NOTION.NOTION_CONNECTOR_ARIA` | `notion-aria` | `trusty/duettoresearch` |
| CTO bot | bot | `NOTION.NOTION_CONNECTOR_CTO_ASSISTANT` | `notion-cto` | `trusty/duettoresearch` |

`NOTION.NOTION_PAT` is never accepted in `bot_ref` (Ddb is resolved by De6).

**Amendment (Dd7).** This spec amends the #8454 "no user token" ruling of 2026-09-23. A user token was refused in every slot; it is now refused in `bot_ref` only and accepted in `personal_ref`. The doc comment on `allowed_refs` in `crates/trusty-channels/src/policy/host.rs` (line 294, "a user token is never accepted (#8454 ruling 2026-09-23)") must cite Dd7 and this spec (`SPEC-CHMODE-04~draft`). The test `slack_user_ref_refused_in_either_slot` (`crates/trusty-channels/src/policy/tests/host_refs.rs:252`) changes to refuse a user ref in `bot_ref` only. Both edits are code work, tracked in the source plan as P3.

## 8. "Sent using @Trusty" footer {#SPEC-CHMODE-08~draft}

**ID:** SPEC-CHMODE-08~draft

Owner rulings: Bob, 2026-10-30 02:38Z (#10499): "We should use a similar convention for the trusty bot channel: 'Sent using @Trusty'." It mirrors Slack's "Sent using @Claude". D58 (Bob, 2026-10-30, #10499): "Footers for bot mode."

- **Rule.** A bot-mode post ends with the footer `Sent using @Trusty`, exactly that text.
- **Scope.** `bot` mode (section 1). `personal` posts carry no footer, per Dda (section 6).

Not yet ruled (OPEN, no decision):

- Per-channel format: a Slack context block or a plain trailing line; the other channels.
- Edits and threads: whether an edited message, or each reply in a thread, carries the footer.
- Opt-out: whether any route or user may turn the footer off.
- Decided (Bob D5c, 2026-10-30): section 6 notes that where a service shows only the owner as author, only the audit line tells proxy from owner. `personal` mode shows no visible marker; the audit line only, as specced (Dda).

Tests: one per channel adapter, asserting a bot-mode post ends with `Sent using @Trusty` and a personal-mode post does not. Refs #10499.
