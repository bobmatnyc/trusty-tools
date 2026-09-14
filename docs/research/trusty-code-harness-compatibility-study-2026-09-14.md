# trusty-code Compatibility Study: pi, opencode, Codex — MCP, Plugins, Agents, and Architecture Lessons

**Status:** Informative (research, not a behavior contract)
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-09-14
**Scope:** MCP config, plugin/agent/skill/command formats, permission models, instruction-file
conventions, and process/architecture design for OpenAI Codex CLI, pi (earendil-works/pi), and
opencode (sst/opencode), mapped against trusty-code's current surfaces and the daemon-first /
PM-only-orchestration / TUI-as-primary-surface direction.
**Related:** #7941, #7942, #5428, #7937, ADR-0060, ADR-0063

## 1. pi (earendil-works/pi)

Read at commit reflected by `pushed_at: 2026-09-14T18:28:46Z` (fetched live via `gh api
repos/earendil-works/pi`); MIT license, 105.1k stars, 13.2k forks, 216 open issues. Monorepo
packages: `pi-coding-agent`, `pi-agent-core`, `pi-ai`, `pi-tui`, `chord`, `pi-telemetry`.
[github.com/earendil-works/pi](https://github.com/earendil-works/pi)

**MCP config.** MCP is **not built into pi core** — it ships as a separate, optional npm
extension, `pi-mcp-adapter` (`pi install npm:pi-mcp-adapter`).
[pi.dev/packages/pi-mcp-adapter](https://pi.dev/packages/pi-mcp-adapter). Files, precedence
lowest→highest: `~/.config/mcp/mcp.json` (tool-agnostic shared) → `~/.agents/mcp.json` /
`~/.agents/mcp/mcp.json` (tool-agnostic alt) → `~/.pi/agent/mcp.json` (pi global) →
`.mcp.json` (project, tool-agnostic) → `.pi/mcp.json` (pi project override, highest). Schema
mirrors Claude Code's `mcpServers` map: stdio (`command`, `args`, `env`, `inheritEnv`, `cwd`,
`lifecycle: lazy`, `idleTimeout`) and HTTP (`url`, `headers`, `auth: oauth|bearer`,
`requestTimeoutMs`). Env substitution: `${VAR}`, `$env:VAR`, or `{env:VAR}` (three accepted
spellings), plus a `!command` shell-exec prefix (`!!` for a literal `!`). Tool naming:
`<server>_<tool_name>` by default, configurable per-server `toolPrefix`
(`server`/`short`/`none`/`mcp`). Trust: config files are read in precedence order with no
automatic discovery of untrusted files; host-config imports require explicit `imports` opt-in
or `/mcp setup`. Tool-level approval via `approveTools` glob patterns (confirm-once /
confirm-for-session / deny), and extensions can broker approval via an event listener.

**Plugin/extension model.** An "extension" is a **TypeScript module**, no separate manifest
file — it exports a default factory `export default function(pi: ExtensionAPI) {}`. Loaded
**in-process** via [jiti](https://github.com/unjs/jiti) (TS executed directly, no compile
step). Discovery: `~/.pi/agent/extensions/*.ts` or `*/index.ts` (global),
`.pi/extensions/*.ts` or `*/index.ts` (project, requires trust), `-e ./path.ts` CLI override,
or `settings.json` `"extensions"` entries. Contributes: tools (`pi.registerTool`), slash
commands (`pi.registerCommand`), keyboard shortcuts, CLI flags, model providers, custom
UI/message renderers, and event-hook interception (`pi.on("tool_call", …)` can `{block:true}`
a dangerous call). Lifecycle events span startup (`project_trust` → `session_start` →
`resources_discover`), per-turn (`turn_start/end`, `tool_execution_start/update/end`,
`tool_call`, `tool_result`, `message_start/update/end`), and session management
(`session_before_compact`, `session_before_tree`, `session_shutdown`).
[raw extensions.md via github.com/earendil-works/pi](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/extensions.md)

**Agents/skills/commands.** No first-class "agent" or "skill" file format in pi core
comparable to Claude Code's `.md`+frontmatter catalogs — UNVERIFIED beyond what
`extensions.md`/`sdk.md` describe (pi's own docs list is `index.md, quickstart.md, usage.md,
models.md, providers.md, extensions.md, skills.md, prompt-templates.md, sessions.md,
session-format.md, rpc.md, sdk.md, compaction.md, security.md, containerization.md,
themes.md`; a `skills.md` exists but its content was not fetched in this pass —
UNVERIFIED). Multi-agent delegation is likewise **not core** — it is example/community
extension territory: `packages/coding-agent/examples/extensions/subagent/index.ts` (an
in-repo example spawning a child `pi` subprocess per subagent, single/parallel/chain modes,
JSON-mode structured output) and third-party packages `pi-subagents`, `pi-agents-team`,
`pi-flows` (Thulr/pi-flows).
[github.com/earendil-works/pi/blob/main/packages/coding-agent/examples/extensions/subagent/index.ts](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/examples/extensions/subagent/index.ts),
[github.com/Thulr/pi-flows](https://github.com/Thulr/pi-flows)

**Permissions/instruction files.** No built-in permission system: "Pi does not include a
built-in permission system for restricting filesystem, process, network, or credential
access. By default, it runs with the permissions of the user and process that launched it."
`AGENTS.md` is read as pi's own project-rules convention (top-level `AGENTS.md` in the repo).
`security.md` layers a "project trust" gate purely over LOADING project-local resources
(`.pi/settings.json`, extensions, system prompts) — asked once, cached, default
`defaultProjectTrust: ask` — explicitly **not** a sandbox: "Project trust is only an
input-loading guard — it doesn't prevent prompt injection risks or make untrusted code safe
after execution begins." Containment is delegated to external containers/VMs.
[github.com/earendil-works/pi security.md](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/security.md)

## 2. opencode (sst/opencode)

Read via [opencode.ai/docs](https://opencode.ai/docs) (docs site reflects the `dev` branch).
MIT license, 207.4k stars, 15,726 commits on `dev`, 4.3k open issues, 1.5k open PRs — very
active. [github.com/sst/opencode](https://github.com/sst/opencode)

**MCP config.** File: `opencode.json`/`opencode.jsonc`, `"mcp"` key, merged across config
levels (local overrides remote/org defaults; "config precedence" is a named, separate doc).
Local (stdio) entries carry: type = local, a command array, an optional cwd, an
environment-variable map, an enabled flag, and a timeout (default tool-fetch timeout
5000ms). Remote entries carry: type = remote, a url, an enabled flag, a headers map, an
oauth block or false, and a timeout. Env substitution: the token form `{env:VAR}`.
OAuth: automatic Dynamic Client Registration by default; explicit `oauth.clientId` /
`oauth.clientSecret` / `oauth.scope` for pre-registered creds, or `oauth: false` to force
API-key mode. Tool naming: MCP tools are "automatically available ... alongside built-in
tools," referenced as `servername_toolname`. Trust: no separate trust gate documented beyond
the general permission system (context-budget warning only — "recommend being careful with
which MCP servers you use"). [opencode.ai/docs/mcp-servers](https://opencode.ai/docs/mcp-servers/)

**Plugin model.** A plugin is a **JS/TS module** exporting one or more plugin functions, types
available from `@opencode-ai/plugin`. Discovery order: global config
(`~/.config/opencode/opencode.json`) then project config (`opencode.json`) then global plugin
dir `~/.config/opencode/plugins/` then project plugin dir `.opencode/plugins/`. Also
installable as npm packages listed under config `plugin: [...]`, auto-installed via Bun into
`~/.cache/opencode/node_modules/`. Contributes: custom tools with **Zod** schemas, hooks
across 30+ lifecycle events (command/file/permission/session/tool/shell/installation/LSP/
messaging/TUI). Runtime: **in-process**, inside opencode's Bun runtime, with context (project
info, cwd, git worktree, SDK client, shell API).
[opencode.ai/docs/plugins](https://opencode.ai/docs/plugins/)

**Agents.** Markdown + YAML frontmatter; filename = agent id (`review.md` becomes `review`).
Locations: `~/.config/opencode/agents/` (global), `.opencode/agents/` (project). Frontmatter:
`description` (required), `mode` (`primary`/`subagent`/`all`), `model` (`provider/model-id`),
`temperature`, `permission` (granular tool gating), `steps` (max iterations), `color`. No
documented `.claude/agents` compatibility reader.
[opencode.ai/docs/agents](https://opencode.ai/docs/agents/)

**Skills.** `SKILL.md`, YAML frontmatter (`name` 1-64 chars lowercase-hyphen, `description`
1-1024 chars, optional `license`/`compatibility`/`metadata`). Locations include **explicit
Claude-compat and tool-agnostic paths**: project `.opencode/skills/<name>/SKILL.md`, global
`~/.config/opencode/skills/`, Claude-compat project `.claude/skills/<name>/SKILL.md`,
Claude-compat global `~/.claude/skills/`, and agent-agnostic `.agents/skills/`. Agents access
skills via a native `skill` tool that lists name+description before loading full body on
demand (progressive disclosure, the same pattern as tcode's
`crates/trusty-code/src/skills/mod.rs:14-33`).
[opencode.ai/docs/skills](https://opencode.ai/docs/skills/)

**Commands.** Markdown, `~/.config/opencode/commands/` / `.opencode/commands/`, filename maps
to `/name`. Frontmatter: `description`, `agent`, `model`, `template`, `subtask`. Body is the
prompt template. No Claude Code slash-command compat documented.
[opencode.ai/docs/commands](https://opencode.ai/docs/commands/)

**Permissions.** `permission` config key, three states per rule (`allow`/`ask`/`deny`), global
(`*`) or per-tool (`read`, `edit`, `glob`, `grep`, `bash`, `task`, `skill`, `lsp`, `question`,
`webfetch`, `websearch`, `external_directory`, `doom_loop`). Object-pattern glob rules with
last-match-wins (e.g. bash sub-rules of `*` = ask, `git *` = allow, `rm *` = deny). Defaults
mostly `allow` except `doom_loop`/`external_directory` (`ask`) and dotenv-style credential
files (denied by default). `--auto` flag auto-approves anything not explicitly denied.
[opencode.ai/docs/permissions](https://opencode.ai/docs/permissions/)

**Instruction files.** `AGENTS.md` native; `CLAUDE.md` read as a **fallback** for migrators.
Precedence: local `AGENTS.md` > local `CLAUDE.md` > global `~/.config/opencode/AGENTS.md` >
global `~/.claude/CLAUDE.md`; "if you have both AGENTS.md and CLAUDE.md, only AGENTS.md is
used." An `instructions` array in `opencode.json` can combine additional files.
[opencode.ai/docs/rules](https://opencode.ai/docs/rules/)

## 3. OpenAI Codex CLI (openai/codex)

**MCP config.** `~/.codex/config.toml` (global); project-scoped `.codex/config.toml` is
honored "trusted projects only." A `mcp_servers.<name>` table per server. Stdio fields:
command, args, env, an `env_vars` allow/forward list (entries can be a bare name or a
`{name, source}` pair where source is local or remote), cwd, and an
`experimental_environment = remote` flag. HTTP fields: url, an auth mode (`oauth` or
`chatgpt`), `bearer_token_env_var`, `http_headers`, `env_http_headers`, and
`http_headers_helper` (a command that returns JSON headers). Universal fields:
`startup_timeout_sec` (default 10), `tool_timeout_sec` (default 60), `enabled`, `required`,
`enabled_tools` / `disabled_tools`, and `default_tools_approval_mode`
(`auto`/`prompt`/`writes`/`approve`). No `{env:VAR}`-style interpolation syntax is documented
— env values are named allow-lists (`env_vars`), not string substitution.
[learn.chatgpt.com/docs/extend/mcp](https://learn.chatgpt.com/docs/extend/mcp?surface=cli),
corroborating community write-up
[vladimirsiedykh.com codex-mcp-config-toml](https://vladimirsiedykh.com/blog/codex-mcp-config-toml-shared-configuration-cli-vscode-setup-2025)
— UNVERIFIED against primary source for the exact field list; treat as best-available
secondary confirmation of the official page's content.

**Approval/sandbox.** `approval_policy`: `on-request` (default in most presets) or `never`;
the former `untrusted` value is **retired** and now blocks startup if present. `sandbox_mode`:
`read-only` / `workspace-write` / `danger-full-access`. Presets: Read-Only (read-only + ask),
Auto (workspace-write + on-request, default inside a git repo), Full Access
(danger-full-access + never, docs label it "extremely risky"). In workspace-write mode,
network is off unless the workspace-write sandbox config sets network access on.
[developers.openai.com/codex/agent-approvals-security](https://developers.openai.com/codex/agent-approvals-security)
(redirect target read via search synthesis — UNVERIFIED direct fetch, corroborated by three
independent secondary sources).

**AGENTS.md discovery.** Global: `$CODEX_HOME` (default `~/.codex`) — an override file if
present, else the base file; only the first non-empty file at that level loads. Project: walk
from the Git root down to the working directory; each directory is checked for the override
name, then the base name, then `project_doc_fallback_filenames` (e.g. `TEAM_GUIDE.md`,
`.agents.md`); at most one file per directory. **Merge**: concatenation root-down, blank-line
joined, so files closer to the working directory are read **last** (highest effective
precedence for conflicts) — structurally identical to Claude Code's root-down concatenation.
**Size cap**: `project_doc_max_bytes`, default 32 KiB; empty files are skipped, and loading
stops once the cap is hit.
[developers.openai.com/codex/guides/agents-md](https://developers.openai.com/codex/guides/agents-md)
(fetched via a `learn.chatgpt.com` redirect target — content is consistent across two
independent search syntheses).

**Skills/plugins.** Skills: a metadata file under `$HOME/.agents/skills` (personal) or
`.agents/skills` (repo) — the **same tool-agnostic path** opencode also reads. Invoked via a
mention trigger, a picker, or an implicit description match. A March-2026 enterprise plugin
layer packages skills, agents, and MCP servers for org-managed distribution via private
marketplaces and policy. UNVERIFIED against a primary OpenAI doc page — sourced from
third-party 2026 guides rather than developers.openai.com directly; treat the plugin-system
claim as lower confidence than the AGENTS.md and MCP findings above.
[github.com/openai/codex/discussions/16329](https://github.com/openai/codex/discussions/16329)

## 4. Instruction-file landscape

| Tool | File(s) | Discovery order | Size limit | Merge semantics |
|---|---|---|---|---|
| **Claude Code** | `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`, `~/.claude/CLAUDE.md`, managed-policy path, `.claude/rules/*.md` | Managed policy, then user (`~/.claude/CLAUDE.md`), then project (root-down, every ancestor dir loaded at launch; subdirectory files load on demand when a file there is read), then `CLAUDE.local.md` appended after `CLAUDE.md` per directory | Target under 200 lines/file for adherence; hard skip over 4 MiB; `@import` max depth 4 hops | **Concatenate**, never override; root-down ordering (ancestor first, cwd last = highest recency); `@path` imports expand inline; external imports (paths outside cwd) need a one-time approval dialog; `AGENTS.md` is NOT read natively — `/import` or an `@AGENTS.md` import/symlink is the bridge |
| **Codex** | `AGENTS.md` plus an override name, plus configurable fallback names | Global (`$CODEX_HOME`, first non-empty of override/base) then project root-to-cwd walk, one file per directory | `project_doc_max_bytes`, 32 KiB default | **Concatenate** root-down, blank-line joined, cwd-nearest read last |
| **pi** | `AGENTS.md` (top-level project convention; no separate global-instructions doc found in this pass — UNVERIFIED) | UNVERIFIED — no discovery-order doc found in this pass | UNVERIFIED | UNVERIFIED |
| **opencode** | `AGENTS.md` (native), `CLAUDE.md` (fallback only) | local `AGENTS.md` beats local `CLAUDE.md` beats global `~/.config/opencode/AGENTS.md` beats global `~/.claude/CLAUDE.md`; extra files via an `instructions` array in `opencode.json` | UNVERIFIED | First-match-wins at each of two tiers (local, global); UNVERIFIED whether opencode walks multiple ancestor directories the way Codex/Claude Code do — docs describe project-root vs. global only |
| **Cursor / Gemini** | Not researched this pass (deprioritized under the time budget) | UNVERIFIED | UNVERIFIED | UNVERIFIED |
| **trusty-code today** | `CLAUDE.md` only, via `crates/trusty-code/src/project_context/mod.rs:38-66` | `<root>/CLAUDE.md` else a `resolve_project_entry` walk of `.trusty-code/` then `.claude/` then `.open-mpm/` (single winning file, `crates/trusty-code/src/paths/mod.rs:86-93`) | `MAX_CONTEXT_BYTES` = 16 KiB, truncated with a provenance note (`project_context/mod.rs:20-27`) | **No merge** — first-found file wins outright; no ancestor walk, no import, no `AGENTS.md` at all |

Key gap: tcode reads exactly one file, has no `AGENTS.md` reader, no ancestor-directory walk,
and no import/concatenation semantics — every one of the four external tools does more here.

## 5. Our current surfaces (path:line)

- `crates/trusty-code/src/paths/mod.rs:86-93` — `SEARCH_ROOTS`: `.trusty-code` then `.claude`
  then `.open-mpm`, one winner via `resolve_project_entry`.
- `crates/trusty-code/src/project_context/mod.rs:38,60-66` — `load_project_context` /
  `locate_claude_md`: single `CLAUDE.md`, 16 KiB cap, no `AGENTS.md`.
- `crates/trusty-code/src/agents/mod.rs:1-11,55-70` — `.md`+frontmatter only (TOML retired
  #2897); `discover_agents` scans `*.md`, falls back to `DEFAULT_AGENTS` (31 embedded) when
  disk yields zero parsed configs.
- `crates/trusty-code/src/agents/config.rs:19-45` — `AgentConfig` groups identity
  (name/role/model/description), LLM params (temperature/max_tokens/model_override), system
  prompt (content/append_skills), and an optional tools allowlist — Claude-Code-shaped
  frontmatter, no `permission`-style per-action verb granularity like opencode's
  `permission.edit`/`permission.bash`/`permission.task` maps.
- `crates/trusty-code/src/skills/mod.rs:14-33` — `.claude/skills/<name>/SKILL.md`,
  progressive disclosure (metadata cheap, body lazy) — architecturally identical to
  opencode's skill model; falls back to `DEFAULT_SKILLS`.
- `crates/trusty-code/src/skills/frontmatter.rs:1-13` — hand-rolled flat key/value fence
  parser (not full YAML), shared by plugin agents/skills (#3539).
- `crates/trusty-code/src/plugins/mod.rs:1-30` — Phase 1 (#3539): local-only
  `<project_root>/.claude/plugins/<plugin>/`, agents+skills only (no commands/hooks/MCP yet),
  namespaced `<plugin>:<name>`, symlink-escape hardening (`path_is_contained`).
- Worktree `crates/trusty-code/src/mcp/` (branch `feat/5428-trusty-code-mcp-loader`):
  - `mod.rs:1-27` — two-tier loader, stdio only this slice, a named constant reports http/sse
    as recognised but not connected.
  - `config.rs:19-34` — `~/.trusty-tools/mcp/servers.toml` (global, ADR-0060) plus
    `<project>/.trusty-code/mcp.toml` (project, via `resolve_project_entry`).
  - `trust.rs:1-30,44-78` — the project tier may only disable a global server or set a
    **content-equivalent** override (byte-identical command/args/env or url/headers); it may
    never introduce a new command — closes the same RCE vector pi's and Codex's
    "trusted projects only" gates close, but ours is content-based, theirs is a trust
    boolean.
  - `tool.rs:35-40` — `composed_name` produces `mcp__<server>__<tool>`, explicitly Claude
    Code's convention.
- `crates/trusty-mcp/src/config/{mod,file,resolve,claude_code}.rs` — ADR-0060 shared
  authority: `~/.trusty-tools/mcp/servers.toml`, the `McpServerConfig`/`McpTransport` types,
  plus a pure `claude_code` converter to and from the map Claude Code reads
  (`crates/trusty-mcp/src/config/mod.rs:16`).

## 6. Compatibility matrix

| Concept | pi | opencode | Claude Code | trusty-code today |
|---|---|---|---|---|
| MCP config file | `.pi/mcp.json` / `~/.pi/agent/mcp.json` (extension-only) | `opencode.json` `mcp` key | `.mcp.json` (project), user settings | `~/.trusty-tools/mcp/servers.toml` (global) + `.trusty-code/mcp.toml` (project) |
| MCP entry schema | `mcpServers`-style JSON (Claude-compat) | typed local/remote object (command or url, env, headers) | `mcpServers` map (command/args/env or url/headers) | TOML `McpServerConfig{name, transport: stdio\|http\|sse}` |
| Env substitution | `${VAR}` / `$env:VAR` / `{env:VAR}` / `!cmd` | `{env:VAR}` | none documented in `.mcp.json` itself | none — no substitution syntax (UNVERIFIED beyond code read) |
| Trust model | precedence order, no auto-discovery of untrusted; glob-based tool approval | context-cost warning only | project `.mcp.json` prompts on first load | content-equivalence gate: project may disable or byte-identical set only (`trust.rs`) |
| Tool naming | `<server>_<tool>` (configurable prefix) | `servername_toolname` | `mcp__<server>__<tool>` | `mcp__<server>__<tool>` (matches Claude Code) |
| Plugin manifest | none — TS default export | none — TS/JS module + optional npm package | `.claude-plugin/plugin.json` | `.claude-plugin/plugin.json` (read, Phase 1) |
| Plugin runtime | in-process (jiti/TS) | in-process (Bun) | in-process (Node, Claude Code itself) | none — Rust, no JS runtime; reads static agent/skill files out of a plugin directory only |
| Agent format | none first-class (extension-defined) | `.md`+YAML, mode/model/permission/steps | `.md`+YAML frontmatter | `.md`+YAML-ish, custom flat parser |
| Skill format | a `skills.md` doc exists, content UNVERIFIED | `SKILL.md`, name/description/license/compat/metadata | `SKILL.md`, progressive disclosure | `SKILL.md`, progressive disclosure (near-identical) |
| Command format | slash commands via `pi.registerCommand` (code, not file) | `.md` file, description/agent/model/template | `.md` file, frontmatter + body | none — no custom-command file format |
| Hooks | 30+ event names, an event-subscription API | 30+ event names, plugin hooks | hooks in settings, shell-command based | none — no hook system found |
| Permissions | none built-in; approval only for MCP tools | `permission` map, 3-state, glob patterns | `permissions.deny`/`allow`, hook-enforced | per-agent tool allowlist only (`AgentConfig.tools`) |
| Instruction files | `AGENTS.md` (root only, UNVERIFIED order) | `AGENTS.md` native, `CLAUDE.md` fallback | `CLAUDE.md` hierarchy, imports, `.claude/rules/` | `CLAUDE.md` only, single file, no import, no `AGENTS.md` |

## 7. Architecture and design lessons

**pi — process model.** Not a daemon. `pi --mode rpc` launches a fresh process per
invocation; JSONL over stdin/stdout with a strict LF-only delimiter (explicitly warns against
Node `readline`, which mis-splits Unicode separators). Docs recommend embedding `AgentSession`
as a library rather than spawning a subprocess for Node/TS callers.
[rpc.md via raw.githubusercontent.com](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/rpc.md)

**pi — session storage.** `~/.pi/agent/sessions/--<cwd-with-seps-dashed>--/<timestamp>_<uuid>.jsonl`.
Every line is an object with type/id/parentId/timestamp forming a **tree** via id/parentId —
branching lives inside one file rather than forking new files. Entry types: `session`
(header), `message`, `model_change`, `thinking_level_change`, `compaction`, `branch_summary`,
`custom`/`custom_message` (extension state, the latter LLM-visible), `label`, `session_info`.
[session-format.md](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md)

**pi — compaction.** Triggers when context tokens exceed the window minus a reserve (default
16,384) or on manual compaction; walks backward to a keep-recent cut point (default 20,000
tokens), summarizes everything before it into a compaction entry (goals/constraints, task
status, decisions, next steps, file read/write tracking; tool results truncated to 2,000
chars). Split-turn summaries merge a "prior context" and "turn prefix" summary when one turn
alone exceeds budget. Branch navigation offers to summarize the abandoned branch into the new
one. Both are interceptable via extension hooks.
[compaction.md](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/compaction.md)

**pi — permissions/TUI.** No sandbox; tools run with the launching user's full permissions
(`security.md`). TUI stack is the in-house `@earendil-works/pi-tui`, TypeScript components,
fullscreen (alt-screen) mode routing normalized mouse/press events vs. a "regular" mode that
leaves terminal scrollback to the terminal itself; diff colors are theme tokens for
added/removed/context.

**opencode — process model.** True **client/server**: `opencode` starts a headless HTTP
server (OpenAPI 3.1, viewable at `/doc`) plus a TUI client that dials it over HTTP + SSE
(`/event`, `/global/event`). `opencode serve` runs the server standalone; IDE plugins drive
the TUI itself remotely through a `/tui` endpoint. Explicitly built to "support multiple
clients ... interact with opencode programmatically."
[opencode.ai/docs/server](https://opencode.ai/docs/server/) — this is the closest of the three
to tcode's own ADR-0058 daemon direction, and the most mature reference implementation of it.

**opencode — TUI stack.** Not ratatui-equivalent: TUI and web share **SolidJS** components
rendered through **OpenTUI**, a separate `sst`-adjacent project — a Zig-native renderer with
framework reconcilers (React/Solid/Vue) sitting behind a TS FFI boundary, cross-compiled to
darwin/linux/win32 x64/arm64 targets. This is a retained-mode, component-tree UI model, not
ratatui's immediate-mode redraw loop — a materially different rendering philosophy from
tcode's planned ratatui+crossterm stack. [opentui.com](https://opentui.com/),
[deepwiki.com/sst/opencode TUI](https://deepwiki.com/sst/opencode/6.2-terminal-user-interface-(tui))

**opencode — provider abstraction.** 75+ providers via the Vercel AI SDK and Models.dev;
addressed as `provider/model-id` (e.g. `openrouter/model-name`); OpenRouter is a first-class
named aggregator; local models (Ollama/llama.cpp/LM Studio) via OpenAI-compatible
custom-provider config (base URL, headers, per-model context/output limits); credentials via
a `/connect` flow stored under the user's local share directory.
[opencode.ai/docs/providers](https://opencode.ai/docs/providers/)

**opencode — multi-agent.** Primary/subagent hierarchy is native (not an extension, unlike
pi): `mode: primary|subagent|all` in agent frontmatter, invocation automatic (a primary picks
a subagent by matching its description) or manual via an `@agent-name` mention.
`permission.task` gates which subagents a given agent may launch by glob. Depth-limited via a
`subagent_depth` setting (default 1: subagents cannot spawn further subagents). Each subagent
call opens a **child session** navigable with parent/child keybinds — session-tree branching,
not raw parallel background execution; running "multiple units of work in parallel" is
described for the general subagent but the parallelism mechanism itself is undocumented.
[opencode.ai/docs/agents](https://opencode.ai/docs/agents/)

**opencode — testing/monorepo.** Bun monorepo (`packages/opencode` = server/core,
`packages/opencode/src/cli/cmd/tui` = SolidJS+OpenTUI TUI, `packages/app` = shared web
components, `packages/desktop` = Electron wrapper, `packages/plugin` = the plugin SDK). The
contributing guide documents no test framework or CI explicitly — only asks PR authors to
describe manual verification.
[github.com/sst/opencode CONTRIBUTING.md](https://raw.githubusercontent.com/sst/opencode/dev/CONTRIBUTING.md)
— a real gap versus tcode's rung-based cargo test ladder.

**Codex — architecture, UNVERIFIED in this pass.** Time budget did not extend to Codex's
process model (daemon vs. CLI-per-invocation), session storage format, or TUI stack — flagged
as a follow-up rather than guessed.

## 8. Lessons ranked by relevance to our direction

Ranked against: daemon-first (ADR-0058), PM-only orchestration with delegation
(`delegate_to_agent`, `crates/trusty-code/src/tools/delegate.rs`), TUI-as-primary-surface
(#7937, `docs/specs/DOC-50-tcode-tui-claude-code-clone.md`), Rust.

1. **Client/server over HTTP+SSE is a proven shape for exactly ADR-0058's target — adopt the
   transport pattern, not the transport.** opencode's server exposes an OpenAPI spec and SSE
   event stream that any client (its own TUI, IDE plugins, third parties) attaches to without
   owning the agent loop; this is structurally what ADR-0058 already specifies for tcode (UDS
   instead of HTTP, per the #6637 ruling). Concretely: model tcode's daemon API surface on
   opencode's global-SSE-plus-per-resource-REST shape rather than inventing a bespoke
   JSON-RPC-only contract, so the eventual TUI (#7937) and a future IDE integration share one
   wire format. Source: `docs/adr/0058-trusty-code-is-an-independent-product-owned-harness.md:63-75`
   (client transport decision) vs. [opencode.ai/docs/server](https://opencode.ai/docs/server/).

2. **Session-tree-with-branches beats session-per-file for a daemon that outlives clients.**
   pi's single-JSONL-file-with-parent-pointer tree (compaction, branch-summary, model-change
   as first-class entry types alongside message) gives a durable task a resumable, inspectable
   history without a database, and lets a detached-then-reattached client (ADR-0058 point 3:
   "detaching a CLI or TUI does not cancel a task") reconstruct exactly where it left off,
   including which branch. Today `SessionRegistry` is in-memory only (ADR-0058 Consequences)
   — this is release-blocking per the ADR itself. Change: give the daemon-owned task store this
   entry-typed, tree-shaped log rather than a flat message array. Source:
   `docs/adr/0058-...md:98-101` ("In-memory session state is insufficient for release
   readiness") vs. [pi session-format.md](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md).

3. **Native primary/subagent hierarchy with depth limits and named task-permission gating is
   more auditable than tcode's current single `delegate_to_agent` tool.** opencode encodes
   delegation depth (`subagent_depth`) and per-agent subagent-invocation allowlists
   (`permission.task` glob) declaratively in the same frontmatter that already carries
   model/tools; tcode's PM-only-orchestration model already forbids agent-to-agent fan-out
   (BASE-AGENT "No Subagent Fan-Out"), but that rule lives in prompt text, not in a config
   field the daemon can enforce independent of the model following instructions. Change: add a
   task-delegation allowlist field to `AgentConfig` so the depth/fan-out rule is mechanically
   enforced, not merely prompted. Source: `crates/trusty-code/src/agents/config.rs:19-45` (no
   such field today) vs. [opencode.ai/docs/agents](https://opencode.ai/docs/agents/).

4. **Tool-call approval needs real granularity: allow/ask/deny, glob-scoped, per verb — tcode
   has only a coarse tool allowlist.** opencode's `permission` map and Codex's
   `approval_policy` plus `sandbox_mode` both separate "may this tool exist" from "may this
   specific invocation run without asking," which tcode's `ToolsConfig{allowed}` cannot
   express (it is all-or-nothing per tool name, no per-argument pattern, no ask tier). This
   matters directly for a TUI-as-primary-surface: the approval UX opencode/Codex both build
   (an ask-dialog per risky call) is the exact interaction #7937 will need to render. Change:
   extend `ToolsConfig` with a glob-pattern verb map before building the TUI's approval card,
   so the TUI renders a real decision rather than a static allow/deny. Source:
   `crates/trusty-code/src/agents/config.rs:110-116` vs.
   [opencode.ai/docs/permissions](https://opencode.ai/docs/permissions/), Codex
   `approval_policy`/`sandbox_mode` (Section 3 above).

5. **Compaction with structured, typed entries, not prose summarization alone, is what makes a
   long daemon-owned task survivable — adopt pi's shape, not just "summarize when near the
   limit."** pi's compaction entry explicitly tracks file read/write provenance and splits a
   compaction across two summaries when a single turn is huge; this is directly relevant
   because a daemon-owned task (ADR-0058) may run far longer than an attached-client session,
   so context management has to be a first-class daemon concern, not a client-side trick.
   UNVERIFIED whether tcode has any compaction today — grep found none in this pass; flag for
   a follow-up code search before building #7937's context view. Source:
   [pi compaction.md](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/compaction.md).

6. **OpenTUI's retained-mode, cross-framework renderer is not a model to copy in Rust, but its
   separation (native renderer core plus a thin per-framework binding) validates keeping
   `trusty-code-tui`'s ratatui/crossterm dependency fully encapsulated, as DOC-50 already
   mandates.** DOC-50's rule that the TUI must not reach around the daemon using
   ncurses/crossterm/ratatui features is the Rust-native version of the same boundary opencode
   enforces by construction (the TUI is a pure HTTP/SSE client with no local filesystem
   access). Confirms the existing plan rather than changing it — cite as validation, not a new
   lesson to act on. Source:
   `docs/specs/DOC-50-tcode-tui-claude-code-clone.md:92,108,152,247` vs.
   [opentui.com](https://opentui.com/).

7. **MCP server pooling belongs to the daemon, not the client, and tcode already does this
   correctly** — spawn once, share client handles across every registry a run builds; opencode's
   own MCP layer does the same thing server-side per opencode instance, not per-client. No
   change needed; cite as convergent validation that MCP lifecycle belongs with the
   daemon/server, matching ADR-0058 point 5 ("Code ... owns process lifecycle for the servers
   and tasks it starts"). Source: worktree `crates/trusty-code/src/mcp/mod.rs:12-19` vs.
   [opencode.ai/docs/mcp-servers](https://opencode.ai/docs/mcp-servers/).

8. **pi's zero built-in permission model and opencode's thin testing story are anti-patterns
   to avoid, not lessons to adopt** — both ship to hundreds of thousands of users on "runs
   with your ambient permissions, be careful" (pi) or no documented test framework (opencode's
   contributing guide). tcode's rung-based cargo test ladder and the content-equivalence MCP
   trust gate (`trust.rs`) are already stricter than either upstream project's own bar — do
   not weaken either to chase compatibility.

## 9. Recommendations

Ranked by value/cost.

1. **Read `AGENTS.md` natively, high value / low cost.** All three researched competitors
   (opencode natively, Codex natively, pi as convention) plus Claude Code's own import bridge
   treat `AGENTS.md` as the cross-tool baseline. tcode reads only `CLAUDE.md` today
   (`project_context/mod.rs`). Add `AGENTS.md` to the read set at the same precedence tier as
   `CLAUDE.md`, following Codex's root-down-concatenate model (Section 4) rather than
   opencode's first-match-only model, since concatenation is strictly more
   information-preserving and matches Claude Code's own semantics. **#5428 slice 2
   implication:** the catalog API/CLI that slice 2 adds for MCP servers should expose an
   analogous "sources" listing for instructions (which files contributed, in what order) so a
   `tcode instructions status` command and a future `tcode mcp status` command share one
   provenance-reporting shape rather than growing two.

2. **Build the owner's distilled-instruction-set reader as a genuinely new capability, not a
   copy of any one tool — cost is real, value is high because none of the four competitors do
   this.** None of pi/opencode/Codex/Claude Code *synthesizes* one canonical instruction set
   from multiple sources; they all pick-and-concatenate. Recommended shape:
   - **Reader set:** `CLAUDE.md` (existing), `AGENTS.md` (new, item 1), `.claude/rules/*.md`
     (Claude Code path-scoped rules — cheap to add, same frontmatter parser tcode already has
     in `skills/frontmatter.rs`), `.cursor/rules/`/`.cursorrules`, and the GitHub Copilot
     instructions file (already enumerated by Claude Code's own `/init` as known-compatible
     inputs — reuse that list rather than inventing a new one). Do **not** attempt pi's or
     Codex's own extension/plugin instruction sources — out of scope, JS-runtime-gated (see
     Risks).
   - **Precedence/provenance model:** record source path, source kind, byte range, and a
     content hash per contributing fragment, mirroring the `ConfigSource`/`McpIssue` pattern
     already used in `paths/mod.rs` and the worktree `mcp/config.rs` — this codebase already
     has the "winning source is a diagnostic" idiom; extend it rather than inventing a
     parallel scheme.
   - **Distillation:** an LLM inference pass (the same PM model, or a cheap one) that
     dedupes/normalizes across sources into one canonical prompt section, similar in spirit to
     a consolidate-and-evict memory pattern — summarize periodically, keep provenance, never
     silently drop a source.
   - **Cache location:** `<project>/.trusty-code/instructions/distilled.md` (generated
     artifact, matches ADR-0058's rule that `.trusty-code/` holds generated adapters,
     manifests, and provenance) plus a sibling manifest file recording per-source hashes for
     invalidation.
   - **Invalidation:** hash-compare each source file's content on every project-context load
     (cheap — files are already read for their raw content); a mismatch triggers
     re-distillation before that source's next PM run, not synchronously on every keystroke.
   - **Auditability:** ship a diff command (CLI or daemon RPC) that renders the distilled file
     against each source's current content, using the manifest's byte ranges — so "why does
     the distilled set say X" is always traceable to a specific source line, the same
     guarantee the MCP config error type already gives config errors.
   - **#5428 slice 2 implication:** slice 2's catalog API/CLI shape (the piece that turns the
     loader into something operators can inspect) is the natural home for this provenance
     surface — design the catalog response schema now so an `instructions` catalog and an
     `mcp` catalog can share one envelope (source tier, path, status, issues) instead of the
     MCP catalog shipping first and the instructions catalog bolting on a second, divergent
     shape later.

3. **Add a glob-pattern per-verb permission map to `AgentConfig`, medium value / medium cost,
   a prerequisite for #7937's approval UX.** See Lesson 4. Do this before, not during, TUI
   build-out, since the TUI needs a real decision object to render a card against. **#5428
   slice 2 implication:** none directly — this is an `AgentConfig` change, not an MCP loader
   change — but the MCP trust gate's `Set`/`Disable` vocabulary (`trust.rs`) is a reasonable
   template for the verb-map's own allow/ask/deny vocabulary, so slice 2 and this change
   should use the same enum-naming convention if they land close together.

4. **Do not build a JS/TS in-process plugin runtime.** Both pi and opencode's plugin models
   require running arbitrary TypeScript in-process (jiti / Bun) — importing that model means
   embedding a JS runtime in a Rust daemon, which is a large, ongoing maintenance and
   security-surface cost with no ADR-0058 driver requiring it. tcode's existing static-file
   plugin ingestion (agents/skills only, `.claude/plugins/`) is the right ceiling; extending it
   to read (never execute) a plugin's declared MCP-server list, if any, is a bounded, low-risk
   next step for #5428 slice 2's catalog — executing a plugin's own code is not.

5. **Consider a pi/opencode-flavored search root only for the instruction-file and skill
   readers (the tool-agnostic `.agents/skills/` path), not for MCP or plugins.**
   `.agents/skills/` is a genuinely tool-agnostic convention both opencode and Codex already
   honor (Sections 2, 3); adding it to `SkillMetadata` discovery (alongside the existing
   `.claude/skills/`) is cheap and gains real interop. Do **not** add a `.pi/` or `.opencode/`
   root to `SEARCH_ROOTS` (`paths/mod.rs`) for MCP config — their schemas (JSON `mcp` key,
   local/remote typed union) diverge enough from `trusty-mcp`'s TOML `McpServerConfig` that a
   reader would need a full second parser and a second trust model for low expected project
   usage; the `.claude/mcp.json`-style Claude Code compat path already covers the common case
   via the existing `claude_code` converter module. **#5428 slice 2 implication:** slice 2's
   loader stays scoped to the two tiers it already has (global `servers.toml`, project
   `mcp.toml`); no third reader is warranted by this research.

## Risks

- **License:** pi and opencode are both MIT — copying their exact JSON/TOML *schemas* (field
  names, shapes) is not a license problem; copying literal prose from their docs into shipped
  product copy would be (attribution risk, not a blocker for a compatibility *reader*). Not
  independently verified against each repo's license file text beyond the GitHub API's
  reported SPDX id.
- **Runtime mismatch:** pi and opencode plugins assume an in-process JS/TS runtime (jiti,
  Bun); trusty-code is Rust with no such runtime today. Any "read pi/opencode plugins"
  recommendation is bounded to *static* file formats (agent/skill Markdown, MCP JSON config)
  — never their executable plugin code. Codex's MCP/skills readers are pure-data
  (TOML/Markdown) and carry no such risk.
- **Codex secondary-source risk:** the developers.openai.com config/approval/agents-md pages
  redirected through a mirror host, and several fetches there 404'd; the Codex findings above
  rest on search synthesis of third-party guides cross-checked against 2-3 independent sources
  rather than a single primary-source fetch. Flagged per-claim above as UNVERIFIED where
  applicable.
- **pi's actual delegation and skill mechanics beyond the subagent example are UNVERIFIED.**
  pi's own `skills.md` content was never fetched in this pass, and every multi-agent pattern
  cited for pi (`pi-subagents`, `pi-agents-team`, `pi-flows`) is a third-party or example
  package, not core — a future revision should pull `skills.md` and the `pi-subagents` README
  directly before treating pi's delegation model as a design input.
- **opencode's ancestor-directory walk for `AGENTS.md` is UNVERIFIED** — the docs describe
  local-vs-global precedence but not whether opencode concatenates every ancestor directory's
  `AGENTS.md` the way Codex and Claude Code do; treat the matrix's "first-match-wins" note as
  the safer reading of the primary text, not a confirmed negative.

## Improvement recommendations

1. **Symptom:** `trusty-code` reads only `CLAUDE.md`, single file, no `AGENTS.md`, no ancestor
   concatenation. **Cause:** `project_context/mod.rs` was built to Claude Code parity only
   (#1033) before `AGENTS.md` became a cross-tool convention. **Change:** add `AGENTS.md` to
   the reader per Recommendation 1; file as a `bobmatnyc/trusty-tools` issue scoped narrower
   than the full distillation system (Recommendation 2), since it is independently shippable
   and immediately raises compatibility with three of four researched competitors.
   **Evidence:** `crates/trusty-code/src/project_context/mod.rs:38-66`.
2. **Symptom:** `AgentConfig.tools` is an all-or-nothing allowlist with no per-verb,
   per-pattern ask/allow/deny tier. **Cause:** built before a TUI needed to render approval UX
   (#7937 postdates the config). **Change:** extend `ToolsConfig` per Recommendation 3 before
   TUI approval-card work starts, so the TUI has a real decision object rather than a boolean
   to render against. **Evidence:** `crates/trusty-code/src/agents/config.rs:110-116` vs.
   `opencode.ai/docs/permissions`.

## Prompt feedback

The task grew across three scope-addition messages mid-run (MCP/plugin matrix, then
architecture/design lessons, then Codex plus the instruction-file landscape), each raising the
line cap but not consolidating what had already been drafted against the new structure — a
single upfront spec with all asks would have let the research pass be planned once (batching
Codex fetches alongside the original pi/opencode fetches) rather than three sequential rounds
of tool calls. Two append calls while writing this file were rejected outright by a
file-secret guard that pattern-matched benign prose (a bold-markdown heading starting with the
letter A, and a compact schema example) as a credential-bearing filename; reformatting the
text around those exact substrings worked, but a large structured document is a plausible
recurring collision with that guard and is worth a narrower trigger condition upstream.
