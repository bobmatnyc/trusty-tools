Added

- **The workstream chat pane appends assistant text live over SSE (streaming
  epic #3696, Slice 3).** Previously the pane only saw a turn once it was
  complete, batch-replacing the whole list every `POLL_MS` from
  `GET /sessions/{id}/transcript`. `WorkstreamActivity.svelte` now also
  subscribes to `GET /sessions/{id}/events` and folds `AgentMessageDelta`
  events into in-progress bubbles rendered alongside the polled entries, so
  text builds up as it is generated. The reducer (`applyDelta` in
  `lib/transcript.ts`) is a pure function — it keys bubbles by
  `(agent_id, turn_id)` rather than `turn_id` alone, so two concurrently
  delegated sub-agents that share a turn counter never interleave into one
  bubble, and it orders bubbles by envelope `seq` so replayed or out-of-order
  deltas still render correctly.

- **Download workstream transcript as Markdown (closes #3526).** The active
  workstream's chat pane header (`WorkstreamActivity.svelte`) gains a
  `download transcript` button that saves the full run transcript — every
  turn, including the tool-run lines (`` `AGENT` ran: write_files ``) that a
  runaway-loop post-mortem needs — as a Markdown file named
  `transcript-<workstream>-<YYYYMMDD-HHMMSS>.md`. The Markdown is rendered by
  the DAEMON (`GET /sessions/{id}/transcript.md`, see the trusty-code
  changelog), so the same document a developer can `curl` in local dev is
  exactly what the button downloads — no second, drift-prone client-side
  serializer. Motivating case: a workstream ran 48 min to
  `deadline_exceeded` with a runaway loop and there was no way to pull the
  transcript to inspect it.
- **CI token drift-check (refs #3486):** a new `scripts/check_token_drift.mjs`
  pins this crate's `app.css` Foundry `--color-*` RGB-triple values to
  `docs/design/UI/design-system/tokens.css` (the canonical hex source),
  wired into CI as the `token-drift` workflow. Fails with a per-token diff if
  the two silently diverge; run locally via `pnpm run check:tokens`. Closes
  the gap `lib/theme.test.ts` left open — that test only checks internal
  light/dark self-consistency, not agreement with canonical. No drift was
  found in this crate — every value already matched canonical.
  Guards against a zero-comparison false-green (an ENFORCED entry with an
  emptied `mappings`/`passthrough` previously reported "matches canonical"
  regardless of the file's real contents — caught by code-critic review) and
  ships `scripts/check_token_drift.test.mjs`, a `node:test` regression suite
  covering drift detection, missing-block/missing-token parse failures, and
  the zero-comparison guard, run in CI ahead of the real check.

- **Agents + Skills management tabs (issue #3449).** Two new nav tabs —
  `AgentsTab.svelte` (replacing the prior per-session-roster stub) and the
  new `SkillsTab.svelte` — list the daemon's full agent/skill catalog with
  tier badges (embedded/bundled = read-only) and add/remove for the
  user-editable disk tier, via a two-step inline confirm on remove
  (matching `WorkstreamSwitcher.svelte`'s pattern). New `lib/agent-roster.ts`/
  `lib/skill-roster.ts` wrap the new `GET`/`POST /agents`,
  `DELETE /agents/{name}` REST routes (and the `skills` equivalent).
  `StartWorkingForm.svelte`'s task form gained an optional agent selector,
  closing the "no pre-task agent roster endpoint exists yet" gap that form
  previously carried as a standing note — omitting a selection still defers
  to the daemon's own default, unchanged from before.
- **Workstream-first creation flow (closes #3365, DOC-48 §8 Phase C+,
  DOC-39 §7A amendment).** Replaces the session-first `CreateSessionForm`
  with `NewWorkstreamForm`: the primary entry control now reads "new
  workstream". Project selection moves into a new `ProjectPickerModal`
  (fed by the daemon's `GET /projects` roster) offering either a known
  project or "start chatting without a project" (maps onto the existing
  projectless/unbound-session state — UI copy stays workstream-neutral
  throughout). Submitting mints a workstream (`POST /workstreams`, named
  from the project + date, or "new chat" + date when projectless), runs the
  first task explicitly bound to it (`POST /tasks` with `workstream_id`),
  and only THEN force-activates it (`POST /workstreams/{id}/activate
  {force:true}` — an explicit user action). Activation is deliberately the
  LAST step, not the second one: a task-run failure after a successful
  create keeps the minted workstream id for the next retry to reuse
  (instead of minting another), and its error message names the workstream
  so the operator knows it already exists. A failed activation after a
  successful run is non-fatal and folded into the success message rather
  than blocking anything.
  `crates/trusty-code-gui/ui/src/components/NewWorkstreamForm.svelte`,
  `ProjectPickerModal.svelte`, `lib/new-workstream.ts` (renamed from
  `lib/create-session.ts`), `lib/project-roster.ts`.
- **GUI workstream switcher in the header (closes #3300, DOC-48 §8 Phase C,
  DOC-39 §2/§8).** New `WorkstreamSwitcher.svelte` fills the reserved header
  slot `AppHeader.svelte`/PR #3301 left as a disabled placeholder: a trigger
  button showing the active workstream's name + state dot, opening a
  dropdown that lists every workstream with an active indicator. Clicking a
  non-active row activates it (`POST /workstreams/{id}/activate`,
  `force: false`); a `409` (DOC-48 §6.1 `ActiveConflict` — another client
  activated concurrently) surfaces an inline banner with a Refresh action
  rather than silently retrying with `force: true`. Each row has inline
  rename (text input, no `window.prompt()`) and close (two-step confirm, no
  `window.confirm()` — mirrors `SessionMonitor.svelte`'s cancel action) —
  close and rename call the new `workstream.close`/`workstream.rename`
  REST routes. Polls `GET /workstreams` every 5s (same
  `$effect`/`AbortController` shape as `StatusBar.svelte`) as the
  authoritative source, plus an `EventSource` subscription to
  `/workstreams/{active_id}/events` for a low-latency nudge on
  `workstream_activation_changed`/`workstream_state_inferred` frames — a
  latency optimization only; there is no daemon-wide "any workstream
  changed" push channel today (a documented API gap, DOC-39 §2.1 C-2), so
  polling never becomes secondary. New `lib/workstreams.ts` (wire types +
  `fetchWorkstreams`/`activateWorkstream`/`closeWorkstream`/
  `renameWorkstream` + pure helpers), unit tested in `workstreams.test.ts`;
  component covered in `WorkstreamSwitcher.test.ts`. `AppHeader.svelte`
  itself is otherwise unchanged — only the reserved slot's content was
  filled in.
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
