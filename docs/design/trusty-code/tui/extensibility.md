# Extensibility — Claude Code TUI

**Status:** Informative (design analysis)
**Last-updated:** 2026-09-06
**Sources:** `code.claude.com/docs/en/{slash-commands,commands,mcp,sub-agents,
output-styles,statusline,ide-integrations}`; `claude plugin --help`,
`claude mcp --help`.

## Slash commands and skills

- The `/` menu lists built-in commands, bundled and user-authored **skills**,
  and commands contributed by plugins and MCP servers, filtered live as you
  type. A few built-ins are deliberately hidden from the menu and only run
  when typed in full.
- Custom commands live as `.claude/commands/<name>.md` (legacy) or
  `.claude/skills/<name>/SKILL.md` (current, recommended — supports
  supporting files in the skill directory and takes precedence on a name
  collision). Both use YAML frontmatter; `argument-hint` drives the
  autocomplete hint; arguments bind via `$ARGUMENTS`, `$N`, or named
  placeholders; skills can stack (`/write-tests /fix-issue 123`).
- `[Skill]`-tagged built-ins observed in the commands table (`/batch`,
  `/claude-api`, `/code-review`, `/dataviz`, `/debug`, `/design`,
  `/design-sync`, `/doctor`, `/fewer-permission-prompts`) confirm skills are
  a first-class mechanism the same table lists alongside true built-ins, not
  a second-tier feature.
- A `[Workflow]`-tagged command (`/deep-research`) shows a third command
  provenance: a Workflow tool script, distinct from a Skill.

## MCP-provided slash commands and resources

- MCP servers can expose `prompts/list` capabilities that surface as `/`
  commands (mcp doc, "MCP-Provided Slash Commands" — e.g. `/database-query`,
  `/slack-send`). These commands look identical to built-ins/skills in the
  menu.
- MCP **resources** (data containers, referenced by URI scheme) are readable
  by Claude without an explicit tool call, distinct from tools and prompts.

## Subagents display

- Subagent files live at `.claude/agents/` (project, VCS-tracked) and
  `~/.claude/agents/` (user, cross-project); both are file-watched with
  seconds-level pickup of edits.
- `/agents` (v2.1.198+) no longer opens a management wizard — it prints a
  reminder to ask Claude or edit those directories directly. `/doctor`
  flags duplicate subagent names.
- Live display: a panel below the prompt shows running subagents as
  `name(task)` rows; nested subagents render as a tree with `(+N)` descendant
  counts; forks (`/fork`, `/subtask`, `/btw`'s `f` key) get one row per fork
  plus one for the main session. `/tasks` is the authoritative full listing
  (running + recently completed + failed, with model/effort per subagent).
- Background subagents needing a permission decision surface the prompt in
  the *main* session, explicitly naming which subagent is asking.

## MCP server status and tool loading

- `/mcp` is the live management panel: connection status glyphs (see
  `screen-inventory.md` §22), auth flows, tool counts, per-server
  enable/disable, per-project persistence in `~/.claude.json`.
- Tool discovery defaults to lazy **tool search** (schemas fetched on demand)
  with a legacy `WaitForMcpServers`-blocking mode as fallback; a remote
  HTTP/SSE server reconnects with exponential backoff (5 attempts, 1s
  initial delay, doubling); interactive sessions show a pending state in
  `/mcp` while reconnecting, `-p`/SDK sessions reconnect silently.
- Per-tool permission surfacing: an MCP tool marked
  `anthropic/requiresUserInteraction` always prompts, in every permission
  mode including `bypassPermissions`, with no "don't ask again" option; an
  org-level connector-tool policy can force an `ask` (prompts even under
  auto modes) or `blocked` (filtered out before Claude ever sees it) status.
- The CLI's own `claude mcp` subcommand surface (add/add-json/
  add-from-claude-desktop/get/list/login/logout/remove/
  reset-project-choices/serve) is the non-interactive management path that
  parallels `/mcp`.

## Plugins

- `claude plugin` (alias `claude plugins`) subcommands: `details`, `disable`,
  `enable`, `eval`, `init|new`, `install|i`, `list`, `marketplace`, `prune|
  autoremove`, `tag`, `uninstall|remove`, `update`, `validate`.
- Session-scoped, non-persistent loading is available via CLI flags:
  `--plugin-dir <path>` (repeatable, directory or `.zip`) and `--plugin-url
  <url>` (repeatable) — loaded for that session only, not installed.
- Plugins can ship output styles in an `output-styles/` directory
  (output-styles doc) — one more artifact type a plugin can contribute
  alongside agents, skills, hooks, MCP servers, and commands (cross-ref
  DOC-51 §1, which specifies trusty-code's own Phase-1 plugin
  agents+skills-only subset of this surface).
- `--restricted` mode strips code-running tools/WebFetch (unless named) and
  ignores user/project/local settings files, a distinct hardening posture
  from a normal plugin-enabled session.

## Status line scripts

- Configured via `/statusline` or a `statusLine` settings key; runs any user
  shell script, receiving session JSON on stdin (model, cost, context %, git
  branch/status, etc. — statusline doc, "Available data"). Multi-line output
  supported. A configured status line suppresses most footer keyboard hints.
- Subagent status lines are a distinct, separately documented mechanism
  (statusline doc, "Subagent status lines") — not itemized further here; see
  `gaps-and-open-questions.md`.

## IDE bridge behavior

- The VS Code extension is "the recommended way to use Claude Code in VS
  Code" and is a **separate GUI surface**, not the terminal TUI running
  inside VS Code's integrated terminal — though the terminal CLI also runs
  there and both can attach to the same IDE session via `--ide` / `/ide`.
- With an IDE attached: plan review/edit before acceptance, auto-accept
  toggle, `@`-mention with specific line ranges from the editor selection,
  conversation history, multiple conversations in separate tabs/windows —
  capabilities layered on top of, not replacing, the terminal interaction
  model.
- `claude --ide` auto-connects to a single available IDE at startup; `/ide`
  manages IDE integrations and shows status from within a running session.
