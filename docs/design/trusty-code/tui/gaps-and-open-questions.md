# Gaps and Open Questions — Claude Code TUI Analysis

**Status:** Informative (design analysis)
**Last-updated:** 2026-09-06

This analysis relied on `claude --help`/subcommand `--help` output (binary
v2.1.263, run non-interactively) and the official docs at
`code.claude.com/docs/en/*`. No interactive Claude Code session was started
during this analysis, per the task constraint — every visual/layout claim
above is inferred from documentation prose, not direct observation. That is
the single largest source of uncertainty below.

## 1. Exact resize/repaint contract

`rendering-and-layout.md` names column thresholds for the diff panel (≥110
opens on demand, ≥144 auto-opens) but the fetched pages did not state a
general terminal-resize behavior (does the transcript reflow live? does a
resize below a width threshold degrade fullscreen to classic mid-session?).
**Settles with:** an interactive session against a real terminal, resized
live, observing reflow — explicitly out of scope for this analysis per its
own constraints.

## 2. Color/theme palette specifics (light/dark/ANSI-16 fallback)

`/theme` and `Ctrl+T` (inside the theme picker) were found, but no page
fetched enumerated the actual palette values, a light-vs-dark toggle
mechanism, or an ANSI-16-degraded-terminal fallback path. **Settles with:**
fetching `code.claude.com/docs/en/theme` or `terminal-config` directly (not
fetched in this pass), or direct observation.

## 3. Notification bell (audible)

No fetched page described an audible terminal bell or OS-level notification
on turn completion/permission-prompt-waiting, though the hooks-guide doc's
example hook ("alerted whenever Claude is waiting for your input instead of
watching the terminal") implies the *feature* exists as a user-configured
hook, not a built-in default. **Settles with:** reading the hooks reference
page for the exact `Notification` hook event, or direct observation of
default behavior with no hook configured.

## 4. Exact PR-badge and spell-check daemon-proxy feasibility

R18/R24 (`requirements.md`) flag that the real product's PR-badge and
spell-check features use local credentials/binaries the thin-client axiom
forbids trusty-code-tui from touching directly. Whether a **daemon-side**
proxy for either is in scope, or whether these two requirements are simply
descoped, is Bob's call, not something this analysis can settle from
documentation alone.

## 5. Subagent status-line mechanism

`extensibility.md` "Status line scripts" notes a documented but unexplored
"Subagent status lines" section in the statusline doc — not read in depth
during this pass (large-page truncation; the fetch tool's preview did not
surface this section's body). **Settles with:** a targeted re-fetch of
`code.claude.com/docs/en/statusline#subagent-status-lines`.

## 6. Full built-in slash-command list

`extensibility.md` and `screen-inventory.md` cite the command table from
`code.claude.com/docs/en/commands`, but that page's fetch was truncated
mid-table (marked `[Content truncated due to length...]` by the fetch tool)
— the alphabetic list stopped partway through the R's. Commands after that
point (anything alphabetically after `/radio`) were not captured.
**Settles with:** paginated re-fetch of the same page, or `claude --help`
combined with in-session `/help` output (not obtainable non-interactively).

## 7. Exact permission-prompt visual layout

`screen-inventory.md` §3's dialog layout (`Accept [a]` / `Don't ask again for
this tool [d]` / `Deny [n]`) is synthesized from the MCP doc's description of
*an* MCP tool-call prompt, not a verified screenshot or ASCII rendering of
the general file/Bash/MCP permission-prompt family. The general
`permissions` doc confirms Tab-for-comment and Left/Right-tab-cycling
behavior but does not itself show the dialog's literal on-screen text.
**Settles with:** direct observation, or the `permissions` doc's own
un-fetched example blocks (the fetch tool summarized rather than quoting
verbatim in places).

## 8. Exact "auto memory" and "recap" visual cues in-session

`state-and-session.md` cites "Saved 2 memories" / "Recalled 2 memories" as
messages the memory doc says appear "in the Claude Code interface" but does
not specify whether these render as transient toasts, inline transcript
lines, or status-line segments. **Settles with:** direct observation.

## 9. trusty-code-tui's current widget-level fidelity vs. this analysis

This analysis did not do a line-by-line diff between
`crates/trusty-code-tui/src/widgets/*` and the requirements in
`requirements.md` beyond what `crates/trusty-code-tui/CHANGELOG.md` states in
prose. A follow-up engineering pass (not a research task) should map each
`requirements.md` MUST/SHOULD item against the actual widget/reducer code to
confirm the DOC-50-status column in `requirements.md` is accurate at the
code level, not just at the spec-prose level.

## 10. `docs/design/trust-code/` (misspelled sibling directory)

Per the task's own note, this directory exists as a sibling with a different
spelling from `docs/design/trusty-code/`. It was confirmed empty
(`find docs/design/trust-code -type f` returned nothing) at analysis time —
there is nothing in it to reconcile against. Flagging only so a future reader
does not assume this analysis silently skipped real prior art there.
