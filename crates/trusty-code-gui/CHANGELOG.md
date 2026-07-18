# Changelog — trusty-code-gui

All notable changes to trusty-code-gui are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Fixed

- Create-session flow response-shape hardening (PR #3103 review findings):
  `GET /fs` 200 bodies and `POST /sessions` 201 bodies are now validated at
  runtime (`lib/create-session.ts::isDirListing` / `extractSessionId`)
  instead of bare `as` type assertions. Previously a 200 listing missing
  `entries` threw a `TypeError` out of the `listing.entries.filter(...)`
  derived — and since `CreateSessionForm` is mounted unconditionally, that
  crashed the entire shell; a 201 missing `id` threw the same way from the
  template's `.slice(0, 8)`. Now a shape-invalid listing degrades to the
  existing picker error line ("malformed response from daemon") and a 201
  without a usable id shows a generic "session created" message (the status
  code, not the body, is authoritative that the session exists). Regression
  tests for both live in `CreateSessionForm.test.ts`; guard unit tests in
  `create-session.test.ts`.
- Search-audit shape guard no longer rejects an entire
  `GET /sessions/{id}/search-audit` response over a single bad record (issue
  #3111 MEDIUM, PR #3110 critic review). `lib/search-audit.ts`'s
  `isSearchAuditResponse` (a `body is SearchAuditResponse` all-or-nothing
  predicate) is replaced by `parseSearchAuditResponse`, which filters
  `search_audit` to the subset passing the existing per-record shape check
  and reports how many entries were dropped (`omittedCount`) — a malformed
  row, or a future third `SearchAuditRecord` variant this client build
  doesn't recognize yet, no longer hides every known-good row behind an
  "audit unavailable" wall. Only a genuine top-level shape failure (body not
  an object, or `search_audit` not even an array) still returns `null` and
  renders that error state. `SearchTab.svelte` now renders the trustworthy
  rows plus a visible "N rows omitted (unrecognized record shape)" notice
  when `omittedCount > 0`, rather than silently dropping or wholesale
  rejecting them. Also added a fake-timer-driven regression test (issue
  #3111 LOW) proving the tab recovers real rows on the next poll after a
  transient `HTTP 500` from the audit route.

### Added

- Create-session flow: the 7a folder picker plus the minimal task-input form
  needed to start a session from the desktop shell (refs DOC-39 §4.2.1,
  §6.2 item 6). Previously the GUI was observe-and-cancel only — there was no
  way to mint a session from the UI at all. `CreateSessionForm.svelte` is a
  pure `fetch()` client over two already-shipped daemon routes: `GET /fs`
  (`fs.list_dir`) for the picker and `POST /sessions` (`session.create`) to
  submit. **No Tauri command and no native dialog plugin were added** — DOC-39
  §2.1 C-4 explicitly bars a Tauri-native fs/dialog as a functional path (it
  would give the Tauri build a capability the web build lacks, which C-3
  forbids), and the daemon-served `GET /fs` route already covers the picker
  identically in both shells. Picker interaction mirrors 7a's own shape:
  browsing a directory lists its children as the selectable candidates (a
  `▸`-style `open` button descends, a `use` button binds without navigating);
  every selectable entry was already confirmed to exist and be a directory by
  the listing call itself, so no separate path-validation round-trip is
  needed before enabling submit. Leaving the selection cleared submits
  projectless (`project` omitted from the body), per AC-2.1's "workstream
  creatable with no project bound" requirement. New `lib/create-session.ts`
  holds the pure logic (`buildCreateBody`, `canSubmitCreate`, `bindingLabel`,
  `describeFsError`), covered by `create-session.test.ts`; the component's
  disabled/enabled submit states, the no-double-submit guard, and picker
  navigation are covered by `CreateSessionForm.test.ts`. Mounted in
  `App.svelte` inside `.body`, between `HealthPanel` and `SessionMonitor`;
  `App.test.ts` gained a new assertion pinning that it renders inside
  `.body`. **Scope gap (documented, not worked around):**
  `GET /sessions/{id}/agents` (`session.get_agents`) requires an existing
  session — there is no pre-session agent-roster route — so this form omits
  `agent` from the `POST /sessions` body entirely and lets the daemon apply
  its own default, rather than inventing a roster endpoint DOC-39 §2.1 C-2
  would call a UI-side workaround.
- Search tab (10d, DOC-39 §4.7) now renders real search/recall audit rows
  instead of the honest "not yet implemented" shell PR #3085 shipped
  (closes #3027; refs #3072, PR #3107). `SearchTab.svelte` polls
  `GET /sessions/{id}/search-audit` for the active session in the same
  `refresh()` cycle that already polls `GET /sessions`, using the identical
  `$effect`/`AbortController`/`setInterval` shape as `StatusBar.svelte` /
  `SessionMonitor.svelte`, plus an independent 1s local age-redisplay tick
  (no network call), mirroring `SessionMonitor.svelte`'s elapsed-time tick.
  New `lib/search-audit.ts` holds the wire types (`SearchAuditRecord`,
  mirroring `crate::events::SearchAuditRecord`'s `search`/`recall` tagged
  variants), a runtime shape guard (`isSearchAuditResponse`, following the
  `create-session.ts::isDirListing` house pattern — a `200` status is a
  promise from this daemon version's handler, not from whatever actually
  answered the socket, so the body's shape is verified before it becomes
  reactive state, never a bare `as` assertion), and three pure per-record
  formatters (`auditLaneLabel`/`auditHitsLabel`/`auditLatencyLabel`) that
  normalize `Search` and `Recall` records onto AC-7.2's shared six columns
  without fabricating fields neither variant carries (`Recall` has no
  `lane`/`latency_ms` on the wire; it renders `'recall'` and an em dash
  respectively, and combines `result_count`/`injected_count` into one
  "N (M injected)" hits label). A malformed response body degrades to a
  labeled "audit unavailable" row rather than throwing; a `404` on the
  audit fetch (session vanished mid-poll, same partial-fetch case
  `SessionMonitor.svelte` already handles for its own chained requests) is
  treated as `no-session`. `search-audit.test.ts` covers the shape guard's
  valid/malformed cases and the formatters; `SearchTab.test.ts` gained
  fetch-URL assertions plus populated/empty/malformed/404/500 audit-state
  coverage on top of the existing four connection-phase tests. **Scope
  note:** this closes the REST-consumption half of the search/recall gap
  #3027 and #3072 shared; `SessionMonitor.svelte`'s own AC-6.3 "settled
  inline card" half has not been wired to this same route yet — tracked
  separately as issue #3108 (that component's gap notice/comment now points
  there instead of the now-closed #3027). Deliberately did not build a
  shared GUI session poller here (issue #3092 tracks that refactor
  opportunity across `StatusBar`/`SessionMonitor`/`SearchTab`) — this card's
  polling stays local, matching the existing per-component pattern.
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
