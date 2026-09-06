# Screen Inventory — Claude Code TUI

**Status:** Informative (design analysis)
**Subsystem:** trusty-code-tui / trusty-code TUI client
**Last-updated:** 2026-09-06
**Sources:** `claude --help` and subcommand `--help` output (binary v2.1.263);
`code.claude.com/docs/en/{interactive-mode,commands,fullscreen,permission-modes,
permissions,mcp,sub-agents,checkpointing,context-window,statusline}`.

Two renderers exist side by side: **classic** (default on older/incompatible
setups, scrollback lives in the terminal's native buffer) and **fullscreen**
(alt-screen, mouse support, `/tui fullscreen` — interactive-mode doc, fullscreen
doc). Most screens below exist in both; where behavior differs it is called out.

## 1. Main transcript

- **Trigger:** default view on session start.
- **Layout — classic:** messages append to the terminal's native scrollback;
  input box floats near the bottom and moves as output streams (fullscreen
  doc, "What changes").
- **Layout — fullscreen:** input box is pinned at the bottom of the alt-screen;
  only visible messages are kept in the render tree (constant memory).
- **Editable:** the input line only; transcript content is read-only (but see
  `/diff` line-selection, §11).
- **Exit path:** `Ctrl+D` twice, `/exit`, or process exit.

## 2. Input composer

- **Trigger:** always present at the bottom.
- **Layout:** multi-line box; grows as text wraps. Right-aligned busy
  indicator while a turn streams (trusty-code-tui's generalized analogue:
  `busy: bool`, no elapsed timer — CHANGELOG "Deliberately NOT ported").
  Shows a grayed-out example prompt on first open, and after a turn a
  next-prompt suggestion (Tab/Right-arrow to accept) — interactive-mode doc,
  "Prompt suggestions".
- **Editable:** full readline-style multi-line text with vim mode option
  (§ interaction-model.md).
- **Exit path:** `Enter` submits; `Esc` interrupts a running turn or closes an
  open dialog; `Ctrl+D` on empty input exits the session.

## 3. Permission prompt

- **Trigger:** a tool call needs approval under the active permission mode
  (permission-modes doc; permissions doc "Permission system").
- **Layout:** modal-like inline dialog — tool name, description, and (per the
  claude-code-guide synthesis of the MCP doc) an options list such as
  `Accept [a]` / `Don't ask again for this tool [d]` / `Deny [n]`. Left/Right
  arrows cycle dialog tabs; `Tab` on Yes/No opens a one-line comment field
  (permissions doc, "Add a comment when you answer a permission prompt");
  `Shift+Tab` with no field open selects the "allow for rest of session"
  option when offered.
- **Editable:** the optional comment field; otherwise choice-only.
- **Exit path:** `Esc` = decline (same as No, no comment); `Enter`/selection
  confirms.

## 4. Plan-mode view

- **Trigger:** `Shift+Tab` cycle, `/plan`, or `--permission-mode plan`
  (permission-modes doc, "Analyze before you edit with plan mode").
- **Layout:** no distinct screen — Claude researches read-only, then presents
  a plan as transcript text followed by a decision prompt: **Yes, and use auto
  mode** / **Yes, and switch to BYPASS PERMISSIONS…** / **Yes, manually
  approve edits** / **No, keep planning**.
- **Editable:** `Ctrl+G` opens the plan text in `$EDITOR` before proceeding.
- **Exit path:** selecting an option exits plan mode into the mode that option
  names; `Shift+Tab` again leaves plan mode without approving.

## 5. AskUserQuestion dialogs

- **Trigger:** the built-in `AskUserQuestion` tool, which always prompts
  regardless of permission mode (permission-modes doc, "Actions no mode
  auto-approves").
- **Layout:** multi-select or single-select menu; a free-text "Other" row
  focuses an input field on click (fullscreen doc, "Use the mouse").
- **Editable:** free-text row when present.
- **Exit path:** submit button / Enter; `Esc` semantics as in §3 for the
  underlying permission gate.

## 6. Tool-call rendering (collapsed / expanded)

- **Trigger:** every tool invocation.
- **Layout — collapsed (default):** a one-line summary, e.g. `Called slack 3
  times` for grouped MCP calls (interactive-mode doc, `Ctrl+O`). Fullscreen:
  click a collapsed result to expand; click again to collapse (fullscreen
  doc, "Use the mouse").
- **Layout — expanded:** `Ctrl+O` toggles the transcript viewer, which shows
  full tool usage, a timestamp, and the model used per assistant message.
- **Editable:** none.
- **Exit path:** `Ctrl+O` again, `q`, or `Esc` (transcript viewer only).

## 7. Diff rendering

- **Trigger:** `/diff`, or automatically once Claude edits a file in a wide
  enough terminal (fullscreen doc, "Watch your changes in the diff panel").
- **Layout — fullscreen "diff panel":** persistent side panel beside the
  conversation; lists changed files with +/- counts, per-file diff below;
  refreshes on every edit/shell command; needs a git repo and ≥110 columns.
  `Ctrl+X B` cycles compare-against scope (this session / uncommitted /
  since branch split).
- **Layout — classic "diff viewer":** replaces the prompt until closed;
  **Current** view (uncommitted changes) plus one turn view per prompt that
  edited files; Left/Right switch views, Up/Down select a file, Enter opens
  it.
- **Editable:** mouse-drag line selection in the panel attaches selected
  lines to the next prompt (fullscreen doc).
- **Exit path:** `/diff` again or the panel's `✕` (fullscreen); `Esc` from the
  file view back to the list, then `Esc` again to close (classic).

## 8. Status line

- **Trigger:** always present when configured (`/statusline`, statusline doc).
- **Layout:** one row above the built-in footer badges; a custom status line
  suppresses most footer keyboard hints (`esc to interrupt`, `? for
  shortcuts`, voice-dictation hint). Renders arbitrary shell-script stdout —
  context %, cost, git branch, model name are common fields (statusline doc,
  "Available data"). Multi-line status lines are supported.
- **Editable:** none directly; configured via `/statusline` or settings.
- **Exit path:** n/a (persistent).

## 9. Spinner / progress and background-task notices

- **Trigger:** a turn in flight; a background Bash command or subagent.
- **Layout:** busy indicator in the input composer (no elapsed timer exposed
  per-doc beyond what the status line script computes itself); background
  subagents appear as rows in a panel below the prompt with a
  `name(task description)` label and a `(+N)` descendant count for nested
  subagents (sub-agents doc synthesis). A completed/failed subagent's row
  persists 30s (`x` clears sooner) with a footer hint `/tasks to see
  subagents`.
- **Editable:** `Enter` opens a subagent's transcript and allows typing to
  resume it; `x` stops the selected one.
- **Exit path:** row disappears on its own after completion + 30s, or `x`.

## 10. `/tasks`

- **Trigger:** `/tasks` command.
- **Layout:** list of running/completed/failed subagents and background
  shells, plus the model (and effort level, v2.1.242+) each subagent runs on.
- **Editable:** selection to view/stop, per sub-agents doc.
- **Exit path:** `Esc`/close command.

## 11. `/agents`

- **Trigger:** `/agents` command.
- **Layout:** as of v2.1.198 this **no longer opens a wizard** — it prints a
  reminder to ask Claude to create/manage subagents or edit
  `.claude/agents/`/`~/.claude/agents/` directly (sub-agents doc). Pre-v2.1.198
  it opened a Running/Library tabbed wizard.
- **Exit path:** immediate (it is a one-shot print in the current version).

## 12. `/artifacts`

- **Trigger:** `/artifacts` command (`commands` doc table).
- **Layout:** list of artifacts owned by or shared with the user; select one
  to attach to the session, open in browser, or copy its link.
- **Exit path:** selection or dismiss.

## 13. `/config`

- **Trigger:** `/config` command, or `/config key=value` for a direct set.
- **Layout:** settings menu — theme, model, output style, editor mode (vim
  on/off), auto-scroll, copy-on-select, prompt suggestions, session recap,
  auto-continue-at-usage-limit, and more (assembled across output-styles,
  fullscreen, and interactive-mode docs' cross-references to `/config`).
- **Editable:** every listed preference.
- **Exit path:** `Esc` / selection.

## 14. `/help`

- **Trigger:** `/help` command.
- **Layout:** list of available commands (interactive-mode doc, "Commands").
- **Exit path:** dismiss.

## 15. `/model`

- **Trigger:** `/model [model]` command, or `Option+P`/`Alt+P` chord.
- **Layout:** model picker if invoked with no argument; direct switch if an
  argument is given. A cache-warning confirmation may appear first
  (interactive-mode doc, "Queue messages while Claude works").
- **Exit path:** selection or Esc.

## 16. `/resume` picker

- **Trigger:** `claude --resume` / `claude -r` with no argument, or `/resume`.
- **Layout:** interactive list of prior sessions, filterable by an optional
  search term (`claude --help`: `-r, --resume [value]`).
- **Exit path:** selection opens that session; Esc cancels.

## 17. Compaction notice

- **Trigger:** `/compact`, or automatic compaction at the auto-compact
  threshold (`--autocompact`, `/autocompact`).
- **Layout:** a **Summarized conversation** marker appears in the transcript
  at the compaction point (checkpointing doc, "Guide a summary"). Root
  CLAUDE.md is re-read from disk and re-injected after compaction
  (memory doc, "Instructions seem lost after /compact").
- **Exit path:** n/a — transcript marker, not a dismissible dialog.

## 18. Cost / usage displays

- **Trigger:** `/cost` (alias for `/usage`), or fields in a custom status
  line script (statusline doc, "Cost and duration tracking").
- **Layout:** text summary of the session's spend; status-line scripts can
  render it persistently.
- **Exit path:** command output, no persistent dialog unless in the status
  line.

## 19. Errors and rate-limit banners

- **Trigger:** a hit usage limit, an auth failure, an `apiKeyHelper` failure,
  etc.
- **Layout:** an inline line, e.g. `Usage limit reached · continuing
  automatically at 3:45pm · esc to cancel` (interactive-mode doc, "Wait for a
  usage limit to reset"); a `Login expires in 3 days` startup warning
  (authentication doc); an `apiKeyHelper` slow-helper warning in the prompt
  bar.
- **Editable:** none; some carry an action hint (`/rate-limit-options`,
  `/login`).
- **Exit path:** self-clears on the triggering condition changing, or `Esc`/
  `Ctrl+C` to cancel an auto-continue wait.

## 20. Rewind menu

- **Trigger:** `/rewind`, or `Esc Esc` on an empty prompt.
- **Layout:** list of every prompt sent this session; per-selection actions
  **Restore code and conversation** / **Restore conversation** / **Restore
  code** / **Summarize from here** / **Summarize up to here** / **Never
  mind** (checkpointing doc). A `Summarize` row can take optional freeform
  guidance text.
- **Exit path:** **Never mind**, or completing an action.

## 21. Transcript viewer (fullscreen-only extras)

- **Trigger:** `Ctrl+O` in fullscreen rendering.
- **Layout:** `less`-style paged view with `/` search, `n`/`N` next/prev
  match, `{`/`}` jump between prompts (fullscreen doc, "Search and review the
  conversation").
- **Exit path:** `Ctrl+O`, `Esc`, or `q`.

## 22. `/mcp` panel

- **Trigger:** `/mcp` command.
- **Layout:** server list with status glyphs (`✔ Connected`, `! Needs
  authentication`, `✘ Failed to connect`, `⏸ Pending approval`, `✘ Rejected`,
  `⊘ Disabled for this project`, cached-with-tool-count), tool counts,
  reconnect/toggle/auth actions (mcp doc synthesis).
- **Exit path:** dismiss / Esc.

## 23. `/focus`

- **Trigger:** `/focus` toggle.
- **Layout:** quieter view — last prompt, one-line tool-call summary with
  diffstats, final response only (fullscreen doc).
- **Exit path:** `/focus` again.
