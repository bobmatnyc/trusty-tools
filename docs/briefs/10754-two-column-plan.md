# #10754 — tm tui MPM pane as two columns: plan

Issue: https://github.com/bobmatnyc/trusty-tools/issues/10754 (builds on #10476). Goal ar-G01.
Owner ask (Bob 2026-11-06 00:47Z): left column owner/project, right column the sessions list,
expand for details, `tm ls` launch commands from the session list.
Base: integ/10476-tui-bundle 9fdf34e23c (installed 1.12.0). Code paths are under
`crates/trusty-mpm/src/tui/project_ctl/`. The D61 freeze holds: commits are local and the merge waits for the reopen.

## Slices

| Slice | What must be TRUE | Rung |
|---|---|---|
| S1 two columns | The left column is owner > project only (still opens expanded). The right column lists the selected project's sessions, with worker indent. `Pane::Sessions` is added; Tab, Left/Right and a click switch columns. Each column has its own selection. The existing k/r/d/a/f verbs act on the right column. | 3 |
| S2 expand in place | Enter or Right on a session row shows its `detail_lines` fields under the row. The expansion is keyed by session id and survives a repoll. Left or Esc collapses it. Mouse hit-testing handles variable-height rows. The modal details overlay is removed. | 3 |
| S3 `tm ls` actions | From a selected session: Enter opens it (resume + attach, as `tm ls` does). `r` renames, `n` starts a new session in the project, `d` deletes with a confirm. Reached through a lib hook registered from the bin, like `tui/focus.rs` FOCUS_HOOK. The TUI suspends and resumes around the attach. S3 may split into S3a (open) and S3b (rename and new). | 5 |

New code goes in new files (`state/session_col.rs`, `panes/session_list.rs`, `tui/open_hook.rs`). `state/mod.rs` is at 494 SLOC, near the 500 cap.

## Changes to shipped behaviour

Session rows leave the left tree. The `fresh_state_shows_every_node_expanded_with_sessions` test changes
accordingly. The details overlay goes away in S2. Owner case-fold, one project per repo, and MPM first are unchanged.

## Open questions for Bob (default in brackets; S1 builds on the defaults)

1. When an owner row is selected, what does the right column show? [All that owner's sessions, grouped under project headers]
2. Should the modal details overlay stay? [Remove it; details expand inline only]
3. `r` is resume here but rename in `tm ls`. [Enter = open (resume + attach); `r` = rename; resume-only moves to `R`]
4. Should opening a session from the TUI suspend the TUI and return to it afterwards? [Yes, the same as `tm ls`]
5. How wide are the columns? [Left 30% with a 24-column minimum; below 60 columns, show one column at a time]
