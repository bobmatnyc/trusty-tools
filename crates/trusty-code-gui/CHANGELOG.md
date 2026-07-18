# Changelog — trusty-code-gui

All notable changes to trusty-code-gui are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

- Phase-1 session monitor card: an active-session summary — status, task,
  elapsed time, a recent transcript tail, and a cancel action — per DOC-39
  §4.6 (the 8b UI surface, refs #2983). `SessionMonitor.svelte` polls
  `GET /sessions` then `GET /sessions/{id}` + `GET /sessions/{id}/transcript`
  every 5s using the identical `$effect`/`AbortController`/`setInterval`
  shape `StatusBar.svelte` established, plus an independent 1s local tick
  (no network call) that redisplays elapsed time via the new
  `lib/transcript.ts::formatElapsed`. Reuses `pickActiveSession`
  (`lib/session-status.ts`) rather than re-deriving session selection; a new
  `SessionDetail` type and exported `TERMINAL_SESSION_STATUSES` constant
  were added there to support it. `lib/transcript.ts::selectTranscriptTail`
  reduces `TranscriptRecord.turns` to the last 5, truncating prose previews
  at 160 chars (mirroring `crate::events::preview`'s convention) and
  rendering tool-only turns as `"ran: toolA, toolB"` rather than a blank
  line. Cancelling requires a two-step in-card confirm (no
  `window.confirm()`) and forces an immediate re-poll on success rather than
  waiting for the next tick. Mounted in `App.svelte` inside `.body`,
  replacing `ActivityPlaceholder` (whose stated purpose — "coming once
  GET /sessions lands" — this card now supersedes); `App.test.ts` gained a
  new assertion pinning that it renders inside `.body`, alongside the
  existing DOC-39 §8.1 AC-18.1 `.statusbar`-is-a-sibling invariant. **Scope
  gap:** DOC-39 §4.6 AC-6.3 literally describes 8b as a live
  search/memory-recall monitor (docked rail → inline settle, preserving
  `lane`/`query`/`hit_count`/`latency_ms`, backed by
  `Event::SearchPerformed`/`Event::MemoryRecalled`). Those events exist in
  `crates/trusty-code/src/events.rs` but are emitted ONLY on the SSE stream
  (`GET /sessions/{id}/events`) with no REST snapshot route and no prior
  SSE-consumption pattern in this client — building that in this slice would
  mean buffering an unbounded per-session event log client-side, which is
  out of scope for a Phase-1 cut. This PR ships the session-activity card
  described above instead and renders a labeled, non-hidden gap notice (same
  treatment as the status bar's `budget: unavailable` span); the true AC-6.3
  gap is tracked in issue #3027.

### Fixed

- Restored the macOS Developer-ID signing config that was intentionally
  omitted at scaffold time, mirroring the `trusty-mpm-gui` pattern (#2951 /
  PR #2957): `tauri.conf.json` now pins `bundle.macOS.signingIdentity` to
  `Developer ID Application: Bob Matsuoka (4JH68XUHC5)` so `cargo tauri build`
  produces a stable-identity `.app` bundle instead of a fresh ad-hoc identity
  per rebuild. `productName`/window title (`"Trusty Code"`) and the bundle
  identifier (`com.trusty.trusty-code.gui`) were already stable from the
  original scaffold and are unchanged. Documented the cert-less
  `APPLE_SIGNING_IDENTITY=- cargo tauri build` escape hatch in the README and
  `docs/reference/common-pitfalls.md`. `trusty-code-gui` was already excluded
  from the workspace's `default-members` at scaffold time (#2983), so no
  change was needed there. `trusty-code`/`tcode` has no `SIGNABLE_BINARIES`
  entry or `tctl sign`-style fallback install script of its own yet, so no
  equivalent install-script wiring was added for `trusty-code-gui` (the Tauri
  config is the only signing path today).

### Added

- Status bar renders the real context budget instead of the "unavailable"
  placeholder described below (refs #3015, DOC-39 §4.5/§6.2).
  `StatusBar.svelte` polls the now-shipped `GET /sessions/{id}/budget`
  (PR #3042, squash `0bec593f`) in the same `refresh()` cycle, sharing the
  existing `AbortController` and post-`await` `signal.aborted` guards, and
  degrades a budget-route failure to a "no data yet" label rather than
  dropping the whole status bar to `daemon-unreachable` — the same
  partial-failure discipline already applied to the readiness fetch. Renders
  `recorded` as `"NN% working"` (appending `", compacted"` when
  `compaction_fired`, DOC-39 §4.5 AC-5.8) and `never_recorded` as a labeled
  `"no data yet"` — never a fabricated `0%`. `within_budget === false` gets a
  distinct red (`bg-status-error`/`text-status-error`) treatment per
  AC-5.7's "red `--gap`" requirement. New `lib/context-budget.ts`
  (`ContextBudgetQuery`/`ContextBudgetSnapshot` wire types plus
  `classifyBudget`/`budgetLabel`/`budgetDotClass`/`budgetTitle`, covered by
  `context-budget.test.ts`) holds the pure formatting/threshold logic,
  mirroring the `lib/session-status.ts` split. **Naming note (issue
  #3043):** the wire types match the shipped Rust field names
  (`working_context_pct`, `compaction_fired`) verbatim, not DOC-39 §5.6's
  stale `working_pct`/`fired` prose — the code is the source of truth.
  **Known gap (issue #3050):** AC-5.9 requires a distinct "not applicable"
  state for non-PM sessions (cadence is PM-only); `ContextBudgetQuery` has
  no PM/cadence discriminant on the wire yet, so non-PM sessions render as
  "no data yet" rather than "not applicable" until #3050 lands the
  discriminant.
- Phase-1 status bar: readiness + budget chrome per DOC-39 §6.2 (refs #2983).
  `StatusBar.svelte` polls `GET /sessions` then `GET /sessions/{id}/readiness`
  (REST Slice 2, squash 15156b42) every 5s via a Svelte 5 `$effect` whose
  teardown clears the interval and aborts a shared `AbortController` (every
  poll's `fetch()` calls carry its signal, and `refresh()` re-checks
  `signal.aborted` after each `await`), so an in-flight poll is genuinely
  cancelled on unmount rather than merely ignored; renders a `daemon-unreachable` /
  `no-session` / `ready` state — same thin-client, no-Rust-proxying pattern as
  the existing `HealthPanel`. `pickActiveSession` (`lib/session-status.ts`)
  is the one piece of client-side logic: with no session picker yet (Phase
  2+ per DOC-39 §6.3) it picks the most recently created non-terminal
  session to reflect. Mounted as `<StatusBar>` in `App.svelte`, structurally
  a **sibling of `.body`**, never nested inside it, per DOC-39 §8.1 /
  AC-18.1 — pinned by a new `App.test.ts` DOM-structure test (`pnpm test`,
  vitest + jsdom, new devDependencies). **Data gap (RESOLVED, see the Added
  entry above this one):** at the time this PR shipped, the budget half of
  the status bar rendered a labeled "unavailable" placeholder —
  `Event::ContextBudget` was emitted on the SSE stream but never cached on
  the session the way `IndexReadinessSnapshot` is, so there was no
  `session.get_context_budget` RPC or `GET /sessions/{id}/budget` REST route
  to poll; tracked as a REST-slice follow-up rather than adding a new daemon
  endpoint in this PR. That follow-up (issue #3015) landed in PR #3042 and
  the placeholder was swapped for live data in this same [Unreleased]
  window.
- Initial scaffold: Tauri 2 + Svelte 5 desktop shell for the `trusty-code`
  (tcode) daemon, mirroring `crates/trusty-mpm-gui`'s structure (refs #2983,
  `docs/specs/trusty-code-harness-ui.md`). A single `get_daemon_url` IPC
  command exposes the configured daemon base URL (`TRUSTY_CODE_URL`, default
  `http://127.0.0.1:7881`); all daemon data (starting with `GET /health`) is
  fetched directly from the frontend via `fetch()`, never proxied through
  Rust, per DOC-39 §2.1's thin-client rule. Minimal working shell only — the
  full DOC-39 screens land in later slices as the REST gateway (#2983) adds
  routes.

### Added

- Phase-1 Search tab (10d, DOC-39 §4.7): `SearchTab.svelte`, mounted in
  `App.svelte` inside `.body` alongside `SessionMonitor`. Per §4.7/AC-7.1,
  this screen is explicitly **not** a search box — it has no `<input>`
  anywhere (pinned by `SearchTab.test.ts`) — and instead is meant to be the
  audit trail of the searches agents performed, rendering AC-7.2's required
  column headers (lane, query, hits, latency, agent, age) as static table
  structure ready to receive rows once the REST gap below closes. Polls
  `GET /sessions` only (identical `$effect`/`AbortController`/`setInterval`
  shape to `StatusBar.svelte`/`SessionMonitor.svelte`) to distinguish
  `connecting`/`daemon-unreachable`/`no-session` from an active session.
  **Scope gap:** `Event::SearchPerformed`/`Event::MemoryRecalled`
  (`crates/trusty-code/src/events.rs`) already carry every field AC-7.2
  needs (`lane`/`query`/`hit_count`/`hits`/`latency_ms`/`agent`/`agent_id`),
  but — the same underlying gap `SessionMonitor.svelte` already called out
  for the 8b card's AC-6.3 (issue #3027) — both events are emitted ONLY on
  the SSE stream (`GET /sessions/{id}/events`) with no REST snapshot/list
  route and no persisted per-session accumulation in the registry. Per this
  PR's architecture rule, the tab must be a thin REST client (never an SSE
  consumer buffering an unbounded per-session event log client-side, and
  never a direct `POST /rpc` caller bypassing the REST resource-gateway
  pattern), so it ships the honest shell described above and renders a
  labeled, non-hidden gap notice (same treatment as `StatusBar`'s `budget:
  unavailable` span) instead of an empty table pretending to be complete.
  Filed as issue #3072, proposing a new `GET /sessions/{id}/search-audit`
  REST route backed by an always-retained `SessionEntry.search_audit` list
  (mirroring the #2962 agents-map precedent), appended from the same
  `SessionRegistry::record_search_performed`/`record_memory_recalled` path
  that already emits the SSE event — a single fix that would close both
  #3027 and #3072. `App.test.ts` gained a third assertion pinning that
  `SearchTab` renders inside `.body`.
