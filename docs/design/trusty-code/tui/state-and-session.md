# State and Session — Claude Code TUI

**Status:** Informative (design analysis)
**Last-updated:** 2026-09-06
**Sources:** `code.claude.com/docs/en/{memory,permission-modes,permissions,
settings,checkpointing,context-window,authentication}`; `claude --help`.

## Session identity, `--resume` and `--continue`

- `-c/--continue`: continue the most recent conversation in the current
  directory (`claude --help`).
- `-r/--resume [value]`: resume by session ID, or open an interactive picker
  filtered by an optional search term (`claude --help`; `screen-inventory.md`
  §16).
- `--fork-session`: on resume, mint a new session ID instead of reusing the
  original.
- `--session-id <uuid>`: pin a specific session id.
- `-n/--name <name>`: display name shown in prompt box, `/resume` picker, and
  terminal title.
- `/branch [name]`: fork the conversation at the current point into a new
  session without disturbing the current one (distinct from `/fork`, which
  detaches to a background session).

## Checkpoints and compaction/context indicators

- Checkpointing mechanics are in `interaction-model.md` ("Rewind and
  checkpoints").
- `/compact [instructions]` frees context by summarizing; `/autocompact
  [auto|<tokens>]` sets the auto-compact threshold (100k–1M tokens or `auto`).
  `/context [all]` visualizes current context usage as a colored grid
  (context-window doc).
- **What survives compaction:** project-root `CLAUDE.md` is re-read from disk
  and re-injected after `/compact`; nested subdirectory `CLAUDE.md`/rules
  reload only when Claude next reads a matching file; a conversation-only
  instruction (never written to `CLAUDE.md`) does not survive.
- A `Summarized conversation` marker appears in the transcript at the
  compaction point (see `screen-inventory.md` §17); fullscreen scrollback
  still holds every pre-compaction message even after repeated compactions.

## Settings precedence

Five layers, highest first: **managed settings** (`managed-settings.json`,
MDM, claude.ai console) → **command line** (`claude --settings`) →
**project local** (`.claude/settings.local.json`) → **shared project**
(`.claude/settings.json`) → **user** (`~/.claude/settings.json`) (settings
doc, "Settings precedence"). Notable per-key exceptions this analysis
surfaced:

- `permissions.defaultMode: "auto"` and `"bypassPermissions"` take effect
  only from `~/.claude/settings.json` or managed settings — the same values
  set in `.claude/settings.json` or `.claude/settings.local.json` are
  silently ignored and the built-in default applies instead
  (permission-modes doc, "Which mode a session starts in").
- `vimInsertModeRemaps` is honored only from user settings, `--settings`, or
  managed settings — a project's `.claude/settings.json`/
  `.claude/settings.local.json` cannot remap keystrokes (interactive-mode
  doc, vim mode section).
- `spellcheck` is read from user settings, `--settings`, or managed
  settings only, and never combined across sources — the first one found
  wins wholesale, fields are not merged.

## Memory files (CLAUDE.md, auto memory)

- **CLAUDE.md load order**, broadest to narrowest scope: managed policy
  (`/Library/Application Support/ClaudeCode/CLAUDE.md` on macOS, etc.) → user
  (`~/.claude/CLAUDE.md`) → project (`./CLAUDE.md` or `./.claude/CLAUDE.md`)
  → local (`./CLAUDE.local.md`). All discovered files are **concatenated**,
  not override-replaced; within a directory, `CLAUDE.local.md` is appended
  after `CLAUDE.md`.
- Nested-directory `CLAUDE.md`/`CLAUDE.local.md` load on demand when Claude
  reads files in that subdirectory, not at launch.
- `@path/to/import` syntax expands recursively (max depth 4); an import that
  resolves outside the working directory in a *project*-level file triggers
  a one-time approval dialog; user-scope imports (e.g. `~/.claude/CLAUDE.md`)
  are trusted without the dialog except in Cowork desktop sessions.
- `.claude/rules/` supports path-scoped rules (`paths:` frontmatter glob) that
  load only when Claude touches a matching file, plus unconditional rules
  loaded at launch alongside `CLAUDE.md`.
- Auto memory is a separate, Claude-authored mechanism: `~/.claude/projects/
  <project>/memory/MEMORY.md` (index, first 200 lines or 25KB loaded at
  session start) plus per-topic files loaded on demand. `/memory` browses and
  toggles it; `/context` confirms what actually loaded.

## Permission modes and their TUI reflection

Six modes, cycled with `Shift+Tab`: `default` (labeled **Manual**),
`acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions`. Full behavior
table:

| Mode | Runs without asking | TUI cue |
|---|---|---|
| `default` (Manual) | Reads only | mode indicator shows "Manual"; every write/exec prompts |
| `acceptEdits` | Reads, file edits, common FS commands | prompts only for shell/network beyond that |
| `plan` | Reads, plus classifier-approved commands when auto mode is available | edits blocked until a plan is approved; approval prompt names the mode it will switch to |
| `auto` | Everything, background classifier review | on Pro/Max/Team this is the **default starting mode**; a one-time startup notice explains it |
| `dontAsk` | Only pre-approved tools | unapproved calls are silently denied, not prompted |
| `bypassPermissions` | Everything | reserved for containers/VMs; still prompts for a narrow "actions no mode auto-approves" list (AskUserQuestion, MCP `requiresUserInteraction` tools, critical-path `rm`, cross-session-messaging safeguards) |

`Shift+Tab` cycles `default → acceptEdits → plan → [bypassPermissions] →
[auto] → default`; `auto`'s first press returns to `default`, not around the
loop. Plan-mode approval offers **Yes, and use auto mode** / **Yes, manually
approve edits** / **No, keep planning**, each switching the session's active
mode. A managed-settings `disableAutoMode: "disable"` removes `auto` from the
cycle entirely and demotes a running auto-mode session to Manual with a
`auto mode disabled by settings` notice.

## Hooks surfaced in the UI

Hooks (`hooks-guide` doc) are shell commands bound to lifecycle events
(`PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `InstructionsLoaded`, etc.).
The TUI's direct visibility into hook execution is thin by design — hooks are
meant to run silently and deterministically:

- `/hooks` shows the configured hook table (per-event bindings), a
  configuration view, not a live-execution log.
- A hook that blocks an action (e.g. a `PreToolUse` "allow"/"deny" hook
  result) surfaces as the corresponding permission outcome, not as a
  separate "hook ran" notice.
- A blocking `UserPromptSubmit` hook that stops an automatic usage-limit
  continuation is reported to the user as "the continuation didn't run"
  (interactive-mode doc) — an indirect surfacing via the affected feature,
  not a hook-specific banner.
- `/debug` + the `InstructionsLoaded` hook are the documented path to
  *actually* observe which instruction files loaded and why — i.e. hook
  visibility is opt-in and debug-log-mediated, not a first-class panel.
