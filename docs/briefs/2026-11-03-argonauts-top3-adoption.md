# Top 3 Argonauts TUI Patterns for tm tui (2026-11-03)

**Source:** `docs/briefs/2026-11-03-argonauts-tui-review.md`  
**Target:** tm tui session grouping ([#10476](https://github.com/bobmatnyc/trusty-tools/issues/10476))  
**Selection criteria:** High value, low integration cost, preserves MPM model

## 1. Request-Based Redraw Architecture

**Source:** `crates/tui/src/runtime/mod.rs:1-3`  
**Pattern:** Redraws are requested, never ticked — idle screen does no work. Uses `tokio::select!` over app events, terminal events, and frame deadlines.

### Value Proposition
Eliminates CPU burn when idle. Current tm tui may tick at fixed interval; this moves to event-driven rendering.

### Acceptance Criteria

- [ ] Event loop uses `tokio::select!` over:
  - MCP events from daemon (session state changes)
  - Terminal events (key presses, mouse, resize)
  - Redraw requests (explicit frame invalidation)
- [ ] Idle screen (no events, no user input) consumes <1% CPU, measured over 60 seconds
- [ ] Frame draw triggered only on:
  - Terminal event (key/mouse/resize)
  - MCP notification (session state update)
  - Explicit redraw request
- [ ] No fixed-interval ticker; no `tokio::time::interval` driving frames
- [ ] Latency p95 <50ms from event arrival to frame drawn, measured under light load
- [ ] Existing tm tui features remain functional (session list, project nav, focus/attach)

### Integration Points

- **File:** `crates/trusty-mpm/src/tui/` (assumed location; verify actual path)
- **Dependencies:** tokio (already present), crossterm or ratatui event loop
- **Breaking changes:** None if event sources stay the same

### Cost Estimate

**LoE:** 2-3 days  
**Breakdown:**
- Day 1: Refactor event loop to `tokio::select!`, remove ticker
- Day 2: Wire redraw requests from MCP handler and terminal handler
- Day 3: Test idle CPU, latency, regression coverage

**Dependencies:** None beyond existing tokio/crossterm

**Risks:** 
- Event source wiring may miss edge cases (e.g., background MCP notification while terminal suspended)
- Performance measurement setup (CPU profiling, latency instrumentation)

---

## 2. Screen Testing Laws

**Source:** `crates/testkit/src/screen/laws.rs:1-61`  
**Pattern:** Five mechanical invariants checked on every test render: selected row drawn, columns aligned (spacing constants), models visually distinct, text not cut when room available, hidden rows counted. `assert_kept()` fails with breach details.

### Value Proposition
High ROI for catching layout regressions. Ensures TUI remains usable across terminal sizes and state variations.

### Acceptance Criteria

- [ ] Test infrastructure captures rendered grid (ratatui `Buffer` or equivalent)
- [ ] Five laws implemented and checked:
  1. **Selected row law:** Selected item is visible in viewport (not scrolled out)
  2. **Column alignment law:** Columns aligned with spacing constants (define for tm: e.g., status=2, name=FILL, time=8)
  3. **Visual distinction law:** Different row types (manager/worker/standalone) visually distinct (color/prefix/indent)
  4. **Text truncation law:** Text not cut when room available (no "session-nam..." when 40 chars free)
  5. **Hidden row count law:** Collapsed groups report hidden row count accurately
- [ ] `assert_kept()` helper fails with clear breach description (which law, which row, expected vs actual)
- [ ] Existing tm tui tests extended with law checks (at least 5 test cases covering different states)
- [ ] Laws pass on all tm tui states: empty, single session, grouped clusters, collapsed groups

### Integration Points

- **New file:** `crates/trusty-mpm/src/tui/testing/laws.rs`
- **Modify:** `crates/trusty-mpm/src/tui/project_ctl/panes/sessions.rs` (add law checks to tests)
- **Dependencies:** ratatui (if using `Buffer` capture), or custom grid abstraction

### Cost Estimate

**LoE:** 4-5 days  
**Breakdown:**
- Day 1: Port grid capture + law framework from argonauts
- Day 2-3: Implement 5 laws for tm tui specifics (spacing, grouping, selection)
- Day 4: Wire into existing tests, add new test cases
- Day 5: Fix any law violations found, refine breach messages

**Dependencies:** 
- Requires access to rendered buffer (ratatui `TestBackend` or equivalent)
- May need test harness refactor if current tests don't capture renders

**Risks:**
- Law definitions need tm-specific tuning (spacing constants, visual distinction rules)
- May surface existing bugs that need fixing before laws can pass

---

## 3. Tab-Based Filtering with Counts

**Source:** `crates/core/src/app/agent_panel.rs:17-61`, `crates/tui/src/views/crew/mod.rs:139-142`  
**Pattern:** Five tabs (NeedsYou, Working, Ready, Done, All) with per-tab counts. Tab cycling via Tab/Shift+Tab. Each tab's `holds()` filters by state.

### Value Proposition
Organizes sessions by state, reduces visual clutter, helps operators find blocked/active work quickly. Fits #10476's "manager/worker clusters" goal.

### Acceptance Criteria

- [ ] Four tabs defined for tm tui states:
  - **Blocked** (pending decision, waiting on input)
  - **Active** (in progress, running)
  - **Ready** (completed, idle)
  - **All** (no filter)
- [ ] Tab header shows name + count (e.g., "Blocked (3)")
- [ ] Tab cycling:
  - Tab key → next tab
  - Shift+Tab → previous tab
  - Wraps at ends
- [ ] Each tab filters session list by state:
  - Blocked: `pending_decision == true` or activity status "waiting_on_input"
  - Active: activity status "in_progress" or "running"
  - Ready: activity status "idle" or status "merged"
  - All: no filter
- [ ] Tab counts update on state change (MCP notification, manual refresh)
- [ ] Selected row preserved when switching tabs (if visible in new tab; else select first)
- [ ] Tab highlight shows active tab visually (color/bold/underline)
- [ ] Keyboard nav (j/k or arrows) works within filtered list

### Integration Points

- **Modify:** `crates/trusty-mpm/src/tui/project_ctl/state/mod.rs` (add tab state, filtering logic)
- **Modify:** `crates/trusty-mpm/src/tui/project_ctl/panes/sessions.rs` (render tab header, apply filter)
- **Modify:** `crates/trusty-mpm/src/tui/project_ctl/events/mod.rs` (handle Tab/Shift+Tab)
- **Dependencies:** State mapping from `SessionRecord` → tm state taxonomy

### Cost Estimate

**LoE:** 3-4 days  
**Breakdown:**
- Day 1: Define tm state taxonomy (blocked/active/ready mapping from `SessionRecord` + `/activity`)
- Day 2: Implement tab state, filtering logic, counts
- Day 3: Render tab header, wire Tab/Shift+Tab keyboard nav
- Day 4: Test state transitions, count updates, selection preservation

**Dependencies:**
- Requires `/activity` endpoint or equivalent for "waiting_on_input" / "in_progress" state (issue #10476 findings mention `pending_decision` + activity)
- State mapping may need new `SessionRecord` fields or derived logic

**Risks:**
- State taxonomy definition needs alignment with PM/operator mental model (what's "blocked" vs "active"?)
- `/activity` fetch may be slow if called for all sessions; need caching or batch fetch

---

## Summary Table

| Pattern | Value | Cost (days) | Risk | Priority |
|---------|-------|-------------|------|----------|
| Request-based redraw | Eliminates idle CPU burn | 2-3 | Low | High |
| Screen testing laws | Regression prevention, high ROI | 4-5 | Medium (may surface bugs) | High |
| Tab-based filtering | User-facing organization | 3-4 | Medium (state taxonomy design) | Medium-High |

**Total LoE:** 9-12 days  
**Recommended order:** 
1. Request-based redraw (foundation for other work)
2. Tab-based filtering (user-visible value)
3. Screen testing laws (QA infrastructure to prevent regressions from 1+2)

**MPM model preservation:** All three patterns are UI-only; no changes to delegation, agent lifecycle, or MCP protocol. tm daemon architecture unchanged.
