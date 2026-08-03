Changed

- **Base font still too small, take two (issue #3447).** PR #3460's 110% root
  bump only reached `rem`-based Tailwind utilities; the ~69 hardcoded
  `text-[9px]`/`text-[10px]`/`text-[11px]` arbitrary-px labels used for
  badges/status/rail/tab text across `WorkstreamRail`, `StatusBar`,
  `StartWorkingForm`, `SearchTab`, `ProjectPickerModal`, `AppHeader`,
  `ServiceNav`, `WorkstreamSwitcher`, `AgentsTab`, `SkillsTab`, and
  `WorkstreamActivity` never scaled since Tailwind emits arbitrary `px`
  literals as-is. Converted every one to its rem equivalent
  (`text-[11px]` -> `text-[0.6875rem]`, `text-[10px]` -> `text-[0.625rem]`,
  `text-[9px]` -> `text-[0.5625rem]`) so they inherit the root multiplier
  like every other token, preserving the relative size hierarchy between
  the three tiers. Root bumped from 110% to 120% (`app.css`) on top of that
  fix for a perceptibly larger base.
- **Workstream/agents UX corrections: hide base agents, PM-only agent choice, immutable project (refs #3465).**
  - The Agents tab and the start-working agent selector no longer list the 5
    `BASE-*` composition-template agents (`base-agent`, `base-engineer`,
    `base-ops`, `base-qa`, `base-research`) — they exist only to be
    `extends:`-ed by concrete agents and were never meant to be dispatched.
    Fixed at the source: `GET /agents` (`crate::agents::protocol::agents_list`,
    see the trusty-code changelog) no longer includes them.
  - **Removed the user-facing agent selector from the start-working bar
    (`StartWorkingForm.svelte`).** "We don't choose agents, the PM does" (Bob).
    The `agent:` dropdown (issue #3449) — and its `agentRoster`/`selectedAgent`
    state and mount-time `GET /agents` fetch — are gone; every `POST /tasks`
    call now omits `agent_name` entirely, so the daemon defaults every run to
    its PM entry agent (`task::protocol::task_run`'s own default, #3437),
    which delegates as needed. The Agents tab itself is unaffected — it still
    lists agents for viewing/managing, this only removes the per-submit picker.
  - **A workstream's project is now locked once the conversation is bound to
    it.** "Once we choose a project for a workstream, we don't change it. New
    workstream if we want to change projects" (Bob). The start-working bar's
    "choose project"/"clear" controls are now shown only BEFORE this
    conversation's first successful submit; once bound (a session or
    continuation-workstream id exists) the project is displayed read-only —
    start a new workstream to use a different project. The daemon already
    enforces the underlying invariant (`task::protocol::task_run` rejects a
    `project` that mismatches an existing session's persisted binding), so
    this is a GUI-only change: the control is removed rather than left to be
    rejected.
- **Branding cleanups.** `ui/index.html`'s `<title>` is corrected from the
  lowercase crate name `trusty-code` to `Trusty Code`, matching the window
  title and header wordmark. `tauri.conf.json` gains an explicit
  `mainBinaryName` of `Trusty Code` so the OS process / bundle binary is no
  longer the raw crate name `trusty-code-gui`. This crate's vendored
  `ui/src/lib/icons/RobotIcon.svelte` has its `aria-label` changed from the
  inherited `Trusty Assistant robot` to the app-appropriate `Trusty Code`
  (canonical design-system source left unchanged).

- **`ProjectPickerModal` marks unregistered (local-only) roster rows
  (issue #3435).** The daemon's `GET /projects` roster is now primarily
  sourced from trusty-mpm's shared project registry; a row without a
  registry match (e.g. a scratch checkout the operator never registered)
  now shows a small "local only" label, following `new-workstream.ts::
  bindingLabel`'s existing state-driven-label precedent. No change to row
  selection/binding behavior. **Update (code-critic PR #3439 review,
  HIGH 2):** when the shared registry itself was unreachable (not merely
  empty), the modal now shows an amber banner — "shared registry
  unavailable — showing local checkouts only" — driven by a new additive
  `source` field on the roster response, so an outage is never mistaken for
  "you have nothing registered."
- **Implicit workstream inference — no creation ceremony (closes #3384,
  DOC-48 §8 Phase C++, DOC-39 §7A/§6.2 amendment).** Bob, after test-driving
  the workstream-first flow below: *"We shouldn't need to 'create' a
  workstream, it should be inferred. Just pick a project and start
  working."* `NewWorkstreamForm.svelte` renamed `StartWorkingForm.svelte` —
  the dedicated "new workstream" section header and "create workstream"
  button are gone; the card now reads "start working" and the submit button
  reads "go". The success message drops the "workstream created — session
  <id>" ceremony text (and the internal session id it exposed) for a plain
  "started". The underlying create → run → activate orchestration (issue
  #3365/PR #3375, incl. the code-critic HIGH fix that made activation the
  LAST step and added `pendingWorkstreamId` retry-reuse) is byte-for-byte
  unchanged — only the UI framing that made minting look like a distinct
  management action is gone. Renaming/closing/switching an existing
  workstream (`WorkstreamSwitcher.svelte`, issue #3300) is untouched.
- **Monitoring reframed around the active workstream (same issue).** Bob:
  *"'SESSION MONITOR — no active session to monitor' — this doesn't make
  sense in the workstream context."* `SessionMonitor.svelte` renamed
  `WorkstreamActivity.svelte` (header: "workstream activity") and re-scoped:
  polls `GET /workstreams` to find the ACTIVE workstream, then selects a
  session from among ONLY that workstream's own bound `session_ids`
  (`pickActiveSessionInWorkstream`, `lib/session-status.ts`) — never a
  session belonging to a different (or no) workstream, closing a real
  correctness gap the prior daemon-wide `pickActiveSession` heuristic had.
  Empty state: **"no active workstream — pick a project to start"**
  (verbatim from the issue); a real active workstream with nothing bound yet
  renders its own "no activity yet" sub-state. Also subscribes to
  `GET /workstreams/{id}/events` (DOC-48 §5.3's SSE aggregation route, issue
  #3343) as a latency nudge on top of the existing REST poll, mirroring
  `WorkstreamSwitcher.svelte`'s identical pattern over the same route — the
  poll stays authoritative.
- **Session vocabulary swept from remaining user-facing GUI text.**
  `StatusBar.svelte` ("no active session" -> "no active workstream", "no
  project bound to this session" -> "no project bound yet", the trailing
  "session &lt;id&gt;" chip -> "id &lt;id&gt;"), `SearchTab.svelte` ("no
  active session — nothing to audit yet" -> "no active workstream — nothing
  to audit yet"), `ServiceNav.svelte` (the Workstream tab's live-dot tooltip
  "session running" -> "workstream active"), `WorkstreamRail.svelte`
  ("start one from the Workstream tab" -> "start working from the
  Workstream tab"). Internal type/variable names (`SessionSummary`,
  `pickActiveSession`, `GET /sessions`, etc.) and API surfaces are
  unchanged — only user-facing labels/empty-states/error strings moved.
  Judgment call: `StatusBar.svelte`/`SearchTab.svelte`/`WorkstreamRail.svelte`
  keep their existing daemon-wide (not workstream-scoped) session-selection
  logic — only `WorkstreamActivity.svelte`'s underlying data source was
  re-architected, since that was the component Bob specifically called out.
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
