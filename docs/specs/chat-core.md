# DOC-20 — Chat-Core: the shared command nucleus for trusty-mpm UIs

**Status:** Draft
**Subsystem:** trusty-mpm — control surface / shared client (`crates/trusty-mpm/src/client/`)
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-19
**Spec ID:** `SPEC-CHAT-CORE-01~draft` (DOC-20)
**Builds on:** DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`,
the `SessionControl` verb names this catalog aligns with); DOC-16 — Interactive
Sessions TUI (`docs/specs/sessions-tui-interactive.md`, the STUI adapter); DOC-19 —
TELUI (`docs/specs/telui-telegram-ui.md`, the Telegram adapter); DOC-18 — Metacoding
(`docs/specs/metacoding-vision.md`, the ONB-3 control surfaces).
**Cross-ref:** the shared client module (`crates/trusty-mpm/src/client/`), the typed
managed `DaemonClient` methods (PR #1491), the SM tool prompt asset
(`crates/trusty-mpm/src/assets/sm_instructions/SM_TOOLS.md`), and issues **#1283**
(chat-core nucleus), **#1433** (TELUI), **#1272** (STUI).

> **Scope note.** This is a **behavior-contract** spec for the **chat-core nucleus** —
> the shared base library every session-manager UI adapter (CLI, TUI, TELUI, future
> Web/Slack) dispatches through. It specifies the layered architecture, the
> catalog-as-single-source-of-truth principle, the resolver contract, and the planned
> adapter rollout. It does **not** implement the adapters; CLI and TUI retain their own
> paths until Phases 1B/1C.

---

## 1. Motivation

Before chat-core, every UI surface (the Telegram bot, the TUI dashboard, the `tm` CLI)
translated operator intent into daemon HTTP calls independently. Three things described
the same verb set and could silently drift:

1. The `/help` body, hand-written in `command.rs`.
2. The typed `TrustyCommand` enum.
3. The SM prompt's `SM_TOOLS.md` prose.

And the same "resolve a fuzzy session reference to a definitive target" logic was
re-implemented at four call sites with subtly different precedence — a latent
correctness bug where the same argument could resolve differently across UIs.

Chat-core collapses all of this into one nucleus: one transport, one intent enum, one
dispatch seam, one result type, one resolver, and one authoritative command catalog.

## 2. Layered architecture {#SPEC-CHAT-CORE-01-layers}

Intent flows top-to-bottom; data flows back up. Every adapter is a thin shell over the
nucleus.

```
┌─────────────────────────────────────────────────────────────┐
│  Adapter (CLI / TUI / TELUI / Web / Slack)                   │  thin: parse → dispatch → render
│    parses native input  ──►  TrustyCommand                   │
│    renders CommandResult ◄──                                 │
└───────────────┬─────────────────────────────▲───────────────┘
                │ TrustyCommand                │ CommandResult
                ▼                              │
┌─────────────────────────────────────────────────────────────┐
│  CommandExecutor::execute  (the single dispatch seam)        │  one match arm per verb
│    sessions.rs · managed.rs · pairing.rs                     │
└───────────────┬─────────────────────────────▲───────────────┘
                │ typed method call            │ typed DTO / Result
                ▼                              │
┌─────────────────────────────────────────────────────────────┐
│  DaemonClient  (the one HTTP transport)                      │  reqwest; URL building; serde DTOs
│    project-session routes · managed-session routes · pairing │
└─────────────────────────────────────────────────────────────┘
```

| Layer | Type | File(s) | Responsibility |
|-------|------|---------|----------------|
| Transport | `DaemonClient` | `client/http_client/` | One `reqwest` client; one typed method per daemon endpoint; serde DTOs. |
| Intent | `TrustyCommand` | `client/command.rs` | UI-agnostic enum; one variant per operator action (project-session, managed, pairing, overseer families). |
| Dispatch | `CommandExecutor` | `client/executor/` | The single `execute(TrustyCommand) → CommandResult` seam; split into `sessions.rs` / `managed.rs` / `pairing.rs`. |
| Result | `CommandResult` | `client/result.rs` | Structured, transport-free outcome; an `Error` variant keeps a dead daemon renderable (never a panic). |
| Catalog | `CommandSpec` | `client/catalog/` (`mod.rs` + `sm_tools.rs`) | The authoritative verb registry (§3). |
| Resolver | `resolve_target` | `client/resolver.rs` | The one fuzzy-reference resolver (§4). |

**Invariant — adapters are thin.** An adapter parses its native input into a
`TrustyCommand`, calls `execute`, and renders the `CommandResult`. It embeds **no** HTTP
logic and **no** resolution precedence. A new daemon endpoint is wired into the nucleus
exactly once and every adapter gains it for free.

## 3. Catalog as single source of truth {#SPEC-CHAT-CORE-01-catalog}

The `catalog` module is the **one** structured description of the verb surface. Every
`CommandSpec` carries a canonical `name`, `aliases`, an `args` sketch, a one-line
`summary`, and a `SpecKind` tying it to the typed command surface (`Command` =
dispatchable; `SmOnly` = SM-prompt-only memory/goals verbs).

Two renderings derive from the same registry, so they can never drift:

- **`help_text()`** renders the operator `/help` body from the `Command` entries. The
  `command::help_text()` shim delegates here, so the help and the dispatch table can
  never disagree.
- **`render_sm_tools()`** renders the `SM_TOOLS.md` prompt section (the three verb
  tables — session control, memory, goals) from the catalog.

**Chosen mechanism — option (b): committed rendering + parity test.** `SM_TOOLS.md` is
kept as a committed asset (so the SM prompt assembly stays a zero-cost `include_str!`
with no runtime allocation, and the prompt-override layering in `core::sm::prompt` is
unchanged). The unit test `catalog::sm_tools::tests::sm_tools_md_matches_catalog_render` asserts
the committed file is **byte-identical** to `render_sm_tools()`. If a verb changes in the
catalog without regenerating the asset (or vice versa), the test fails — so the prompt
and the code can never silently drift. The catalog is authoritative: to change the SM
tool surface, edit the catalog and regenerate the asset; the test enforces it.

> Option (a) — generating the prompt section from the catalog at use time — was rejected
> only to avoid touching the audited `core::sm::prompt` assembly/override floor and to
> keep prompt assembly allocation-free. The parity test gives option (b) the same
> no-drift guarantee.

## 4. The resolver contract {#SPEC-CHAT-CORE-01-resolver}

`resolve_target<T: Resolvable>(items, query) -> Option<&T>` is the **one** fuzzy-reference
resolver. `Resolvable` exposes `id_matches(query)` (a predicate, so a UUID newtype
compares in place without allocating) and `resolve_name()`. Precedence is fixed:

1. **id-exact** — the first item whose id equals `query`.
2. **name-exact** — the first item whose friendly name equals `query`.
3. **unambiguous name-prefix** — the prefix match **only when exactly one** item's
   non-empty name starts with `query`. An ambiguous prefix (≥2 matches) resolves to
   `None` — a UI never silently picks the wrong session. An empty `query` never matches.

The trait is implemented for both `SessionRow` (project sessions) and
`ManagedSessionSummary` (managed sessions), so one precedence definition serves both
families. The executor's project-session `resolve_session` and the managed verbs both
delegate here.

**Phasing note.** Phase 1A provides the canonical resolver and adopts it inside the
executor. The CLI's `classify_managed_target` / `resolve_managed_id` / `resolve_session_id`
resolvers adopt it in **Phase 1B** — they are intentionally untouched in 1A.

## 5. The verb surface {#SPEC-CHAT-CORE-01-verbs}

`TrustyCommand` covers the full managed verb set so any adapter can express every
operation, alongside the retained project-session/pairing/overseer family:

| Verb (command) | `TrustyCommand` | `DaemonClient` method | `SessionControl` alignment |
|----------------|-----------------|-----------------------|----------------------------|
| `managed-new` / `spawn` | `ManagedNew` | `spawn_managed_session` | `sessions.launch` |
| `managed-list` | `ManagedList` | `list_managed_sessions` | `sessions.list` |
| `managed-get` | `ManagedGet` | `get_managed_session` | `sessions.get` |
| `managed-send` | `ManagedSend` | `send_to_managed_session` | `sessions.send` |
| `managed-answer` | `ManagedAnswer` | `answer_managed_session` | `sessions.send` (decision) |
| `managed-activity` | `ManagedActivity` | `managed_session_activity` | `sessions.get` (observe) |
| `managed-attach` | `ManagedAttachCmd` | `managed_session_attach_cmd` | — |
| `managed-stop` | `ManagedRuntimeStop` | `runtime_stop_managed_session` | `sessions.stop` |
| `managed-resume` | `ManagedResume` | `resume_managed_session` | `sessions.resume` |
| `managed-decommission` | `ManagedDecommission` | `decommission_managed_session` | `sessions.kill` |

### 5.1 Decision-answer correctness {#SPEC-CHAT-CORE-01-decision}

A managed session's decision is answered through the **real**
`POST /api/v1/sessions/managed/{id}/answer` endpoint (`answer_managed_session`), **not** a
synthetic `PostToolUse` hook. The former synthetic-hook path in `decide()` was a
correctness gap: it audited a decision the harness never actually received, so the
session stayed blocked. Now:

- `ManagedAnswer { target, answer }` posts the answer directly to the answer endpoint.
- `/approve` / `/deny` (the `Approve`/`Deny` commands the Telegram bot uses) resolve the
  target against the managed list first; on a match they answer via the real endpoint
  (approve → the session's `proposed_default`, else `"yes"`; deny → `"no"`). The Telegram
  surface transparently gets the corrected behavior — its `TrustyCommand`/`CommandResult`
  contract is unchanged. A non-managed project session keeps its lightweight
  existence-check-and-acknowledge behavior (there is no managed decision to answer), with
  the synthetic hook removed entirely.

## 6. Planned adapter rollout {#SPEC-CHAT-CORE-01-rollout}

| Phase | Scope | Status |
|-------|-------|--------|
| **1A** | Build the nucleus: full `TrustyCommand` verb set, canonical `resolve_target`, executor split (`sessions`/`managed`/`pairing`), real decision-answer, catalog SoT + parity test. **Telegram keeps working unchanged.** | This spec / PR |
| **1B** | Route the `tm` CLI (`bin/tm/commands/managed.rs`, `session.rs`) through chat-core; adopt `resolve_target` at those call sites. | Planned |
| **1C** | Wire the TUI (STUI) slash-dispatch through `CommandExecutor`. | Planned |
| **2** | Make the HTTP `/api/v1/sessions/chat` endpoint action-capable; Web adapter. | Planned |
| **future** | Slack adapter (peer of TUI/TELUI per DOC-18 §ONB-3). | Planned |

Each adapter is a thin presentation/control layer over the **same** nucleus; none
re-implements the session-manager engine, the inference layer, or the lifecycle logic.

## 7. Non-goals (Phase 1A)

- Rewiring the `tm` CLI resolvers (1B) or the TUI slash-dispatch (1C).
- Making `/api/v1/sessions/chat` action-capable (Phase 2).
- Any change to the SM prompt **content** — `SM_TOOLS.md` is byte-identical to its prior
  text; only its provenance (catalog-rendered, parity-tested) changes.
