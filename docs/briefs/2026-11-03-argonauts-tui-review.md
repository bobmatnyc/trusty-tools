# Argonauts TUI Review (2026-11-03)

## Executive Summary

Argonauts Code implements a sophisticated TUI with request-based redraws, comprehensive testing infrastructure, and hierarchical session grouping with retry folding. The agent panel's tab-based organization with repo/run grouping and the screen-testing "laws" system are high-value patterns for `tm tui` #10476. The event architecture and input handling are well-designed but tightly coupled to their orchestration model.

## Findings by Priority

### Adopt (high value, low integration cost)

- **Request-based redraw architecture**: `crates/tui/src/runtime/mod.rs:1-3` — "Redraws are requested, never ticked, so an idle screen does no work". Uses `tokio::select!` over app events, terminal events, and frame deadlines. Eliminates CPU burn when idle. **LoE: 2-3 days** to refactor `tm tui` event loop. No dependencies beyond tokio.

- **Event burst handling**: `crates/tui/src/runtime/screen.rs:47-48` — Processes up to 256 events (`EVENT_BURST`) before drawing a frame, so rapid updates cost one frame not many. Currently at `screen.rs:289-318` in main loop. **LoE: 1 day** to add burst batching. Requires measuring impact on perceived latency under load.

- **Tab-based filtering with counts**: `crates/core/src/app/agent_panel.rs:17-61` — Five tabs (NeedsYou, Working, Ready, Done, All) with per-tab counts. Tab cycling via Tab/Shift+Tab at `crates/tui/src/views/crew/mod.rs:139-142`. Each tab's `holds()` filters by state at `agent_panel.rs:49-60`. **LoE: 3-4 days** for tab UI, state mapping, and keyboard nav. Requires defining tm-specific state taxonomy (blocked, running, ready, done).

- **Retry folding**: `crates/core/src/app/agent_panel.rs:197-240` — Multiple tries of same agent shown as one row with ordinal count ("accepted on its 3rd try"). `fold_tries()` matches by label key and start/end times. Agent name kept from first try. **LoE: 2 days** to implement. Depends on session/round tracking in tm state model.

- **Terminal capability detection**: `crates/tui/src/runtime/mod.rs:64-88` — Probes background color, graphics protocol (Kitty), pixel mouse, cell size at startup with 250ms timeout each. Graceful degradation when unanswered. Used to enable conditional features. **LoE: 1-2 days** for detection; 3-5 days if graphics support added. No immediate need for Kitty graphics in tm.

- **Screen testing laws**: `crates/testkit/src/screen/laws.rs:1-61` — Five invariants checked mechanically: selected row drawn, columns aligned with spacing constants (WORDS=4, NUMBERS=3, BARS=3), models visually distinct, text not cut when room available, hidden rows counted. `assert_kept()` fails with breach details at `laws.rs:165-178`. **LoE: 4-5 days** to port grid capture + law checks. High ROI for catching layout regressions.

- **Real-size test matrix**: `crates/testkit/src/screen/mod.rs:39-44` — Every panel drawn at 4 real terminal sizes: 120×38 (laptop), 177×40 (wide), 80×24 (classic), 265×60 (large monitor). Ensures usability across devices. **LoE: 1 day** to add size matrix to existing tests. Low cost, high confidence gain.

### Adapt (valuable but needs translation to MPM model)

- **Hierarchical grouping by repo → run**: `crates/core/src/app/agent_panel.rs:370-395` — Rows grouped first by repo (home repo first, others by urgency), then by run within each repo. Run groups show heading with state/spend. Method `groups()` builds hierarchy at `agent_panel.rs:368-436`. **Adaptation needed**: tm has no "repo" concept; substitute "project" or "worktree". Run → session/round mapping. **LoE: 5-6 days** for grouping logic + heading rendering. Requires defining tm group keys (project path? session ID? issue number?).

- **Urgency-based sorting**: `crates/core/src/app/agent_panel.rs:438-440` — Sorts by `(state, Reverse(updated))` so most urgent (by state enum order) and newest appears first. State order: NeedsYou < Working < Waiting < Ready < Done < Stopped < Failed. **Adaptation needed**: Map tm states (blocked, running, ready, done) to urgency order. **LoE: 2 days**. Depends on state taxonomy from tab filtering.

- **Details pane beside or under list**: `crates/tui/src/views/crew/mod.rs:35-36` — At ≥120 columns, details show beside list; narrower terminals show under list with Space toggle. Layout at `draw.rs` (not shown but referenced). **Adaptation needed**: tm details may differ (session log excerpt? PM notes? blocking condition?). **LoE: 3-4 days** for layout switching + detail rendering. Requires defining detail schema.

- **Bridge pattern with AppState/AppEvent/UiCommand**: `crates/tui/src/bridge/mod.rs:1-6` — UI receives `AppEvent`s from controller, sends `UiCommand`s back. `Bridge` applies events to `AppState`, routes to handlers, collects `Effects`. Clean separation. **Adaptation needed**: tm's daemon communication differs (MCP calls? Unix socket?). Bridge useful as abstraction but needs tm-specific wire protocol. **LoE: 1 week** to design state sync protocol + implement bridge. Large dependency on tm daemon architecture decisions.

- **Input routing with priority layers**: `crates/tui/src/app/input.rs:1-11` — Keys routed in priority order: transcript overlay → history search → settings picker → focused question → agent panel → menu → vim modes → global keys → composer. Each layer can consume or pass through. **Adaptation needed**: tm's modal hierarchy may differ (no composer, different overlay types). Pattern valuable but needs tm-specific layer list. **LoE: 3-4 days** to define tm input layers + routing logic.

- **Keymap with chord support**: `crates/tui/src/app/input.rs:266-305` — Two-key chords like Ctrl+X Ctrl+S with 1-second window (`CHORD_WAIT`). First key held, second completes binding. Configurable via `Keymap` at `crates/tui/src/base/input/keymap.rs`. **Adaptation needed**: tm may not need chords initially (simple single-key nav sufficient for #10476). **LoE: 2-3 days** if chords wanted; defer otherwise.

- **Question/modal handling with draft preservation**: `crates/tui/src/app/input.rs:89-116` — When question takes keys, draft input set aside and restored after answer. Views auto-close except those allowed over questions. Keys typed during transition dropped. **Adaptation needed**: tm has no "questions" in argonauts sense (approval prompts). Pattern useful for future blocking dialogs. **LoE: 2-3 days** when needed. Not immediate priority.

### Skip (not applicable or high cost/low value)

- **Picture/graphics streaming with worker thread**: `crates/tui/src/world/deliver/mod.rs:1-10, 103-171` — Off-frame PNG encoding on background thread, Kitty graphics protocol, LRU picture cache with size-based eviction. Used for animated mascot/world view. **Reason to skip**: tm tui is text-only; no graphics planned. High complexity (worker thread, protocol, cache management) for no benefit. Would add 1000+ LoC.

- **Fullscreen alternate screen mode**: `crates/tui/src/runtime/screen.rs:224-239` — Can use alternate screen to hide conversation, show only live region. Toggled via settings. **Reason to skip**: tm already uses inline mode where history scrolls into terminal scrollback. Alternate screen would break copy/paste of prior output. Preference: keep inline. **LoE to add: 2-3 days** but adds mode complexity.

- **Editor handover for drafts**: `crates/tui/src/runtime/editor.rs` — Opens `$VISUAL` or `$EDITOR` on draft, suspends TUI, resumes after editor exits. **Reason to skip**: tm tui is for monitoring/navigation, not composition. No draft editing needed for #10476 scope. **LoE: 2 days** but no use case.

- **Council/CLI session tracking**: `crates/tui/src/council/` — Tracks multiple agent CLI sessions (Codex, Claude Code, etc.) with per-session state. **Reason to skip**: tm has one daemon, not multiple agent CLIs. Council abstraction not applicable. **LoE to adapt: 1+ week** for marginal value.

- **Vim mode throughout UI**: `crates/tui/src/bottom/vim.rs` and composer vim mode — Full vim keybindings (insert/normal modes, hjkl nav, etc.). **Reason to skip**: Not in #10476 scope. Standard arrow keys sufficient initially. Can revisit post-launch if user demand. **LoE: 1 week** to implement vim mode properly.

- **Accessibility/screen reader mode**: `crates/tui/src/a11y/` — Alternate non-visual UI rendering events as plain text for screen readers. **Reason to skip**: Important for production but not in #10476 scope. Defer to accessibility-focused effort. **LoE: 2+ weeks** for proper implementation.

- **Transcript overlay on alternate screen**: `crates/tui/src/app/overlay.rs` — Ctrl+O opens scrollable history on alternate screen, Esc returns. **Reason to skip**: Terminal scrollback already provides this. Overlay adds complexity without clear advantage over native terminal scroll. **LoE: 3-4 days** but duplicates existing capability.

## Integration Notes

**Dependencies:**
- Tab filtering + retry folding require defining tm state taxonomy (blocked/running/ready/done mapping).
- Grouping needs decision on group key: project path, worktree name, session ID, or issue number.
- Bridge pattern requires tm daemon state sync protocol design.

**Risks:**
- Argonauts model assumes one screen per controller; tm may have multiple tmux sessions per daemon. Need multi-session safety.
- Event burst batching may increase perceived latency if burst size too large. Start with 64, measure, tune.
- Screen testing laws assume ratatui `Buffer`; tm may use different rendering. Grid capture adapter needed.

**Unknowns:**
- How does tm track session start/end times for retry folding? Does round metadata include timestamps?
- What details should detail pane show? Session log tail? Blocking condition? PM instructions?
- Does tm need repo-level grouping or is worktree/project sufficient?

**Recommended phasing:**
1. Phase 1 (1-2 weeks): Request-based redraw + event burst + tab filtering + retry folding
2. Phase 2 (1-2 weeks): Hierarchical grouping + urgency sort + details pane
3. Phase 3 (1 week): Screen testing laws + real-size matrix
4. Defer: Terminal capability detection (no graphics needed), input routing (simple nav sufficient), bridge pattern (design state sync first)

**Code pointers for implementation:**
- Event loop: `runtime/screen.rs:220-340` (main loop with select)
- Tab filtering: `agent_panel.rs:49-60` (state predicates), `views/crew/mod.rs:139-142` (tab cycling)
- Retry folding: `agent_panel.rs:197-240` (fold_tries with LabelTry)
- Grouping: `agent_panel.rs:368-436` (groups method building RepoGroup/RunGroup hierarchy)
- Screen laws: `testkit/src/screen/laws.rs:154-178` (check + assert_kept)

**File count scanned:** 47 Rust source files across `crates/tui/src/`, `crates/core/src/app/`, `crates/testkit/src/screen/`

**Total LoE estimate for Adopt items:** 16-21 days
**Total LoE estimate for Adapt items:** 25-30 days (includes design time for state sync protocol)
