# Changelog — trusty-code-gui

All notable changes to trusty-code-gui are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Fixed

- **`DEFAULT_DAEMON_URL` moved from `http://127.0.0.1:7881` to
  `http://127.0.0.1:7882`, in lockstep with `trusty-code`'s
  `serve::DEFAULT_HTTP_PORT` (closes #3364).** The old port collided with
  `trusty-mpm`'s supervisor metrics listener, producing "DAEMON UNREACHABLE
  — LOAD FAILED" on a fresh install when both processes were running. A new
  test, `default_daemon_url_matches_tcode_default_http_port`, pins this
  constant against `trusty_code::serve::DEFAULT_HTTP_PORT` directly (new
  dev-dependency on `trusty-code`) so the two cannot drift apart silently
  again. The web-mode fallback — `ui/src/lib/api-config.ts`'s own
  `DEFAULT_DAEMON_URL`, used by `apiBase()` outside the Tauri webview (the
  `pnpm dev` loop and any plain browser tab) — carried a THIRD copy of this
  port that the initial #3364 fix missed; it is now 7882 too, with
  `ui/src/lib/api-config.test.ts` parsing `trusty-code/src/serve/mod.rs`
  directly to assert the two stay in sync.

### Added

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

### Changed

- Shell rebuild to the DOC-39 §8 Foundry skeleton (closes #3153). `App.svelte`
  was a flat `.body` stack of cards (`HealthPanel`/`CreateSessionForm`/
  `SessionMonitor`/`SearchTab`); it is now the real one-flex-column shell:
  `.hdr` (new `AppHeader.svelte` — brand, a read-only workstream label plus a
  disabled placeholder control reserving the workstream-switcher slot DOC-48
  (PR #3284) will fill, and a tokens/cost stub) → `.body` (`.wsrail`, new
  `WorkstreamRail.svelte` — a 240px/46px collapsible rail with a
  Workstream|Project segmented toggle over a card list, `--trusty-sidebar-*`
  chassis tokens ported from `docs/design/UI/design-system/tokens.css` — plus
  `.actpane` = `.wsnav`, new `ServiceNav.svelte`, the 7 canonical tabs
  (Workstream/Project/Agents/Memory/Search/Workflow/Files) — and `.actbody`,
  the per-tab content host) → `.statusbar`, still `.body`'s sibling per the
  AC-18.1 nesting invariant (unchanged, still pinned by `App.test.ts`).
  `HealthPanel.svelte` is removed — its sole signal (daemon reachability) is
  already reported by `StatusBar`'s own `daemon-unreachable` phase.
  `CreateSessionForm`/`SessionMonitor` now mount inside the Workstream tab
  (new `WorkstreamTab.svelte`); `SearchTab` mounts as the Search tab's
  content, unchanged otherwise. New `lib/nav-tabs.ts` (`tabVisibility`, unit
  tested in `nav-tabs.test.ts`) is the pure decision logic behind two nav
  rules the build brief calls out: AC-4.2 — every project-scoped tab renders
  **locked, not hidden** (disabled + tooltip) while the workstream is
  projectless; AC-9.4 — Workflow is the one exception, **hidden** (not
  locked) unless a project is bound to an actual git repo (Phase 1 has no
  wire field for an existing session's git-repo-ness, so Workflow stays
  hidden until a real `project.status`-style route exists — a documented API
  gap, not a guess). `StatusBar.svelte` gains the PROJECT and AGENTS-mode
  segments the brief's status-line spec calls for (`workstream state ·
  PROJECT binding · SEARCH readiness · MEM · AGENTS mode · TOKENS · COST`),
  fetching `GET /sessions/{id}` alongside its existing readiness/budget
  calls; MEM and COST have no daemon route yet (#3181, #3254) and render an
  honest stub rather than fabricated data. Project/Agents/Memory/Workflow/
  Files tabs ship as Foundry-style empty-state stubs (idle mark + mono label
  + one line of guidance naming the gap) — no mockup content for these lands
  in this structural pass. **Scope note (not completed, documented rather
  than silently dropped):** the Workstream tab does not implement mockup
  8a's literal thread+docked-rail ~16/37/28 split — there is no thread/chat
  surface anywhere in this codebase yet (a separate, unbuilt feature), and
  8a's docked rail specifically needs the SSE consumer tracked as #3251,
  which the build brief itself defers. Goal slots (DOC-39 §4.5) are also not
  wired in this pass, left as a follow-up slice per the brief's own
  "engineer's judgment, appears in no mockup" framing.
- Renamed a `CreateSessionForm.test.ts` case (PR #3250 critic review,
  cosmetic MEDIUM): `'surfaces the per-call project-mismatch 400 from the
  daemon verbatim'` implied the component special-cases a project mismatch.
  It doesn't — it's the same generic `error.message` passthrough the
  preceding test already covers, just replayed with a project-mismatch
  payload as real-world example content. Renamed to `'surfaces an arbitrary
  daemon 400 error message verbatim, exercised with a project-mismatch
  payload'`.

### Fixed

- GUI-created sessions were inert (closes #3177): the create-session form
  called `POST /sessions` (`session.create`), which only ever minted a
  session record — it never spawned an agent loop, so nothing typed into the
  form actually ran. The form now calls `POST /tasks` (`task.run`, #2983
  Slice 4), the one-shot "mint-or-reuse a session AND start executing" entry
  point (DOC-39 §7A/AC-21), carrying the per-call `project` binding the same
  way (PR #3189's project mismatch `400` surfaces via the form's existing
  generic error-message handling, unchanged). `lib/create-session.ts`'s
  `buildCreateBody`/`extractSessionId` are renamed `buildRunTaskBody`/
  `extractTaskSessionId` to match the new wire shape (`task_description` +
  `project`; response `{session_id, status, mode, binding}`, `202 Accepted`
  instead of `201 Created`), with a new `isTaskRunResponse` runtime shape
  guard replacing the old `POST /sessions`-shaped check.

### Changed

- Foundry retheme (refs #3153, DOC-39 addendum AC-27): the placeholder
  slate/indigo Tailwind palette is replaced wholesale with the now-normative
  Foundry design system (`docs/design/UI/design-system/`) — rust-on-paper
  light theme, "Night Shift" dark theme. `tailwind.config.js` keeps the
  `rgb(var(--color-*) / <alpha-value>)` CSS-variable plumbing #3133
  established (still the only way Tailwind can generate the
  `bg-status-ok/15`-style opacity modifiers every component relies on) but
  the token set grew from four bare colors to the full Foundry set
  (`trusty-primary(-hover)`, `trusty-surface`, `trusty-card`,
  `trusty-raised`, `trusty-border(-strong)`, `trusty-text(-secondary|-muted|
  -inverse)`, `status-ok/error/warn/neutral`) plus a `fontFamily` mapping for
  the three Foundry faces and an extended `borderRadius`/`borderWidth` scale
  (3/5/8px radii, 1px dividers / 1.5px containers, per the design system's
  guardrails). `src/app.css` carries the actual light/dark RGB-triple values.
  **Activation reconciliation:** Foundry activates dark via
  `<html data-theme="dark">`, not a bare `prefers-color-scheme` media query —
  new `lib/theme-bootstrap.ts` bridges OS appearance to that attribute
  (`initThemeBootstrap`, called once from `main.ts` before the shell mounts),
  with a `matchMedia` `change` listener for live OS-appearance switching.
  System-following remains the only behavior (no manual toggle, no persisted
  preference — both explicitly out of scope for this pass). No CSS-only
  `@media` fallback was kept: `<body>` has no paintable content before
  `main.ts` mounts `App.svelte`, so there is nothing for a pre-JS flash to
  show, and a second value-defining mechanism would only risk the two
  drifting apart. **Fonts are bundled locally**, not linked from Google
  Fonts: `ui/public/fonts/` now carries the same self-hosted IBM Plex
  Sans/Mono + Chakra Petch woff2 files (OFL-1.1, license text alongside)
  already vetted for `crates/trusty-agents/ui/public/fonts/`, loaded via
  `@font-face` rules inline in `index.html` — required for this Tauri app to
  render correctly fully offline and to avoid ever needing a remote-origin
  CSP allowance. All five components (`App`, `StatusBar`, `HealthPanel`,
  `SessionMonitor`, `SearchTab`, `CreateSessionForm`) were restyled to
  Foundry: 1.5px card borders, raised header strips (`bg-trusty-raised`) on
  every card, button tiers per the design system's quick reference (primary
  solid rust / secondary card / tertiary raised / danger soft-rust — never
  two transparent buttons side by side), rectangular mono badges (never
  pills), and machine-readable text (ids, counts, labels, table headers) in
  `font-mono` uppercase tracking-wide, reserving `font-display` (Chakra
  Petch) for headings. No component hardcodes a hex color — every color
  routes through a `trusty-*`/`status-*` token. `lib/theme.test.ts` is
  rewritten for the new token set/values and the attribute-based activation
  mechanism (asserting `[data-theme='dark']`, not
  `@media (prefers-color-scheme: dark)`), plus new coverage for
  `theme-bootstrap.ts`'s immediate-apply/change-listener/no-matchMedia-guard
  behavior and the font self-hosting invariant; the existing no-raw-color
  and no-`<style>`-block hygiene scans are preserved and extended with a
  hardcoded-hex scan.

### Fixed

- GUI smoke-test UX feedback (Bob, 2026-07-18): three fixes to
  `CreateSessionForm.svelte`/`HealthPanel.svelte` and the global theme.
  - **#3132 Enter-to-submit.** The task `<textarea>` had no keyboard submit
    path — click was the only way in. `docs/specs/trusty-code-harness-ui.md`
    specifies no submit-key convention, so this follows the universal
    textarea pattern: `Enter` (without `Shift`, not mid-IME-composition)
    calls `preventDefault()` and submits; `Shift+Enter` is left untouched,
    falling through to the textarea's own newline-insertion default. The
    existing `canSubmitCreate` no-double-submit guard is unchanged — the new
    `handleTaskKeydown` handler defers to the same `submit()` the button
    already called, adding no separate gate. Covered by three new
    `CreateSessionForm.test.ts` cases: Enter submits (and a rapid second
    Enter while the first is in flight produces no second `POST`),
    Shift+Enter never calls `preventDefault()` or submits, and a non-Enter
    key is a no-op.
  - **#3133 light + dark themes following system appearance.** Every
    `trusty-*`/`status-*` Tailwind color token (`tailwind.config.js`) now
    resolves to `rgb(var(--color-*) / <alpha-value>)` instead of a
    hardcoded hex literal; the actual light (default) and
    `@media (prefers-color-scheme: dark)` (override) values live in
    `src/app.css`. No manual toggle — theming purely follows the OS
    setting, live, including inside the Tauri webview (WebKit re-evaluates
    the media query on the native "Appearance changed" notification with no
    reload needed). The previous `darkMode: 'class'` config was dead
    weight — nothing in this codebase ever added a `.dark` class to
    `<html>`, so its `html:not(.dark)` override in `app.css` silently won
    every render regardless of the OS setting. The CSS-variable values are
    space-separated RGB triples (`"15 23 42"`, not `"#0f172a"`) — required
    by the `rgb(var(...) / <alpha-value>)` format, the only way Tailwind
    can still generate the `bg-status-ok/15`/`text-trusty-text/60`-style
    opacity modifiers used throughout every component; a bare `var(--x)`
    reference (this PR's first cut) compiles the base utility fine but
    silently produces NO rule at all for any opacity-modified one. The
    theming audit also caught one real hardcoded color outside the token
    system: `HealthPanel.svelte`'s JSON payload `<pre>` block used the
    Tailwind built-in near-black shade (which never changes with
    `prefers-color-scheme`) instead of a themed token — swapped for the
    `trusty-border` token at reduced opacity. New `lib/theme.test.ts`
    covers the token format, both themes' CSS-variable values (and that
    dark genuinely differs from light), a real `postcss`/`tailwindcss`
    compile check proving opacity-modified utilities produce a rule (the
    actual regression a bare `var()` causes), and a component-hygiene scan
    for `<style>` blocks, hardcoded inline-style colors, and raw
    (non-themed) Tailwind palette utilities.
  - **#3134 picker placement.** The directory listing's `use` button sat at
    the row's far right (`flex-1` on the name button + `justify-between` on
    the row stretched the name button to fill the available width), leaving
    a wide, disorienting gap between a short directory name and the control
    that binds it. The name button is now width-bounded
    (`min-w-0 max-w-[65%]`, `truncate` still applies) rather than
    flex-stretched, so with `justify-between` removed the row's default
    left-packed flex layout puts `use` immediately after the name. Covered
    by a new `CreateSessionForm.test.ts` structural assertion: the `use`
    button must be the name button's next DOM sibling, and neither the name
    button nor the row may carry the `flex-1`/`justify-between` classes that
    caused the separation.
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
