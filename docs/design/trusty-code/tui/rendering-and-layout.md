# Rendering and Layout — Claude Code TUI

**Status:** Informative (design analysis)
**Last-updated:** 2026-09-06
**Sources:** `code.claude.com/docs/en/fullscreen` (primary), `interactive-mode`,
`statusline`.

## Transcript structure

Two renderers hold the transcript differently:

- **Classic:** the conversation lives in the terminal's own native scrollback.
  `Cmd+F` / tmux copy-mode search it directly. Memory grows with conversation
  length because rendered content accumulates in the native buffer.
- **Fullscreen:** the conversation lives in the alt-screen buffer, off the
  terminal's native scrollback. Only currently-visible messages are kept in
  the render tree, so memory stays flat regardless of conversation length.
  Native search tools see nothing there by default — `Ctrl+O` (transcript
  mode) then `[` writes the full expanded conversation into native scrollback
  on demand, or `v` opens it in `$VISUAL`/`$EDITOR`.

## Markdown rendering

Not itemized exhaustively in the fetched docs; observed conventions: code
fences render with syntax highlighting (toggle inside the `/theme` picker via
`Ctrl+T`, "Theme and display" table); assistant text otherwise renders as
formatted markdown inline in the transcript.

## Code block and diff styling

- Diff content has two dedicated presentations, not just syntax-highlighted
  fences — see `screen-inventory.md` §7 (diff panel / diff viewer). The
  fullscreen diff panel shows added/removed line counts per file and can
  render a mouse-selectable diff region that attaches selected lines to the
  next prompt.
- The diff panel explicitly **collapses** test files and generated files, and
  folds pre-session changes into one expandable summary line — a deliberate
  truncation policy, not a limitation.

## Tool-result truncation and expansion

- Grouped/repeated MCP calls collapse to one line by default (`Called slack 3
  times`); `Ctrl+O` (transcript viewer) expands these and any other
  by-default-collapsed line, e.g. cross-session message previews
  (`Message from @<sender>`).
- In fullscreen, a collapsed tool result is individually click-to-expand /
  click-to-collapse (mouse), independent of the global transcript-viewer
  toggle.

## Colour and theme handling

- `/theme` picker exists; `Ctrl+T` inside it toggles syntax highlighting for
  code blocks specifically (light vs dark/ANSI-16 palette selection itself
  was not itemized in the fetched pages — see
  `gaps-and-open-questions.md`).
- Spell-check underline color is themeable (`spellcheck.color`), defaulting
  to the active theme's error color.
- Status-line output is arbitrary ANSI from the user's script — the host
  imposes no palette on it beyond terminal capability.

## Terminal resize

Not directly documented in the fetched pages beyond the general expectation
that fullscreen rendering "works at any window size" (the fullscreen doc's
own clarification that "fullscreen" describes alt-screen takeover, not window
maximization). Diff-panel and prompt-suggestion behaviors are column-gated
(≥110 cols for the diff panel to open via `/diff`, ≥144 to auto-open,
≥80 assumed elsewhere per DOC-50's own SSH/narrow-terminal note) — see
`gaps-and-open-questions.md` for the exact resize-repaint contract.

## Scrollback

- **Classic:** native terminal scrollback; no in-app paging model beyond what
  the terminal itself provides.
- **Fullscreen:** `PgUp`/`PgDn` (half-screen), `Ctrl+Home`/`Ctrl+End`
  (jump to start / jump to latest + re-enable auto-follow), mouse wheel.
  Auto-follow pauses on manual scroll-up; a floating "Jump to bottom" button
  shows a new-message count; disable auto-follow entirely via `/config` →
  Auto-scroll.
- Scrolling back past a `/compact` boundary still shows every earlier message
  in fullscreen scrollback (Claude itself only sees the compaction summary
  going forward).

## Width thresholds

- Diff panel: needs a git repo, fullscreen rendering, and ≥110 columns to
  open via `/diff`; auto-opens on its own once Claude edits a file only in a
  ≥144-column terminal.
- No other exact width threshold was found in the fetched pages for general
  transcript layout; DOC-50 §4.2/§9 Q3 names an SSH/narrow-terminal fallback
  (<80 cols) as a real product concern for trusty-code-tui, unconfirmed
  against the real product's own exact cutoff — flagged in
  `gaps-and-open-questions.md`.

## Notification bell / title-bar updates

- A PR-review-state badge appears in the footer once a branch has an open PR
  (`PR #446`), color-coded by review state (green=approved, yellow=pending,
  red=changes-requested, gray=draft) and disappears on merge/close; same
  mechanism for GitLab (`MR !N`). `Cmd/Ctrl+click` opens it in the browser.
  This refreshes on `git push` / `gh pr`·`glab mr` state-changing commands
  succeeding in-session.
- Terminal title updates: the CLI accepts `-n/--name` to set "a display name
  … shown in the prompt box, /resume picker, and terminal title" (`claude
  --help`), confirming the title bar is actively managed, though the exact
  update triggers beyond session start were not directly documented in the
  fetched pages.
- No explicit terminal-bell (audible) notification behavior was found in the
  fetched pages; see `gaps-and-open-questions.md`.

## Foundry design-system relevance

trusty-code-tui is a terminal (ratatui) UI, so Foundry's CSS/token system
(`docs/design/UI/README.md`) does not apply literally. Where it transfers:
the **palette relationship** (status/semantic colors — ok/warn/error/accent)
and the **robot-mark idle/working/receiving states** are conceptually
reusable as ANSI-256/truecolor equivalents and as spinner/status-glyph
states, respectively, but no CSS token maps directly onto a ratatui `Style`.
This is a design note, not a requirement — no requirement below claims a
literal Foundry-to-ratatui mapping exists today.
