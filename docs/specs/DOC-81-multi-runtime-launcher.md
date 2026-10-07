# DOC-81 — Multi-Runtime Launcher: Runtime Adapter Bundle for Codex, OpenCode and Gemini CLI

**Status:** Draft (proposed; epic not yet filed)
**Spec ID:** `SPEC-RUNTIMES-01~draft` … `SPEC-RUNTIMES-06~draft` (DOC-81)
**Subsystem:** `trusty-mpm` — `runtime/`, `session_manager/`, `core/` launch, settings, pane-cue and deploy modules; `bin/tm` CLI; daemon spawn and resume routes.
**Owner:** Bob Matsuoka
**Last-updated:** 2026-10-05
**DOC-N claim:** `DOC-81`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). The catalog note names `DOC-79` as next free, but open PR [#9176](https://github.com/bobmatnyc/trusty-tools/pull/9176) adds `DOC-79` and `DOC-80`. No tracked file on `origin/main` (de478389ef) and no other open PR names `DOC-81` (checked 2026-10-05). Re-verify before merge.
**Product requirements:** [PRD-RUNTIMES-01](../prd/PRD-RUNTIMES-01-multi-runtime-launcher.md)
**Builds on:** [ADR-0059](../adr/0059-canonical-agent-behavior-has-generated-host-adapters.md); [DOC-78](./DOC-78-claude-code-mods-integration.md) (Claude Code hook surface).
**Cross-ref:** epic [#7342](https://github.com/bobmatnyc/trusty-tools/issues/7342) (Cursor stays there), [#7343](https://github.com/bobmatnyc/trusty-tools/issues/7343), [#5418](https://github.com/bobmatnyc/trusty-tools/issues/5418).

Code citations are permalinks pinned to `de478389ef`. Link text is `path:line`; paths are relative to `crates/trusty-mpm/src/`. Every cited line was read at that commit.

Evidence for runtimes other than Claude Code comes from official docs read on 2026-10-05, listed in [Annex A](#annex-a-sources). **UNCONFIRMED** means no official source was found. A UNCONFIRMED mark stays until an empirical capture (§6, item E) replaces it with a dated, versioned fact. Nothing in this spec promotes one.

---

## 1. Context {#SPEC-RUNTIMES-01~draft}

**ID:** SPEC-RUNTIMES-01~draft
**Status:** Draft

### 1.1 What exists

`RuntimeAdapter` ([`runtime/mod.rs:101`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/mod.rs#L101)) has three methods: `spawn` (:119), `spawn_resume` (:152) and `identify` (:172). `RuntimeKind` ([`runtime/mod.rs:191`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/mod.rs#L191)) has `ClaudeCode` and `Tcode`. `build_adapter` ([`runtime/mod.rs:270`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/mod.rs#L270)) maps one to the other. `ClaudeCodeAdapter` is at [`runtime/claude_code.rs:723`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/claude_code.rs#L723), `TcodeAdapter` at [`runtime/tcode.rs:182`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/tcode.rs#L182).

The trait cannot host another runtime. It carries a Claude-named parameter (`claude_session_id`), and every other Claude Code control lives outside it, spread across `core/` and `session_manager/` (§2.3).

### 1.2 Relation to ADR-0059

ADR-0059 makes host content layouts (`.claude/`, `.codex/`, tcode) generated adapters of one canonical source. It governs **what is deployed**: agents, skills, instructions.

This spec adds the **runtime-control adapter**: how a runtime is started, steered, observed and resumed. The two are separate seams.

| | Content adapter (ADR-0059) | Runtime-control adapter (this spec) |
|---|---|---|
| Output | Files in host layouts | argv, env, hooks config, pane behavior |
| Selected by | Host | `RuntimeKind` |
| Owns | Agent, skill and instruction rendering | Launch, permission, resume, cues, config home |
| Boundary | The control adapter calls the content adapter through one operation, `install_assets` (§2.2). It does not render content itself. |

### 1.3 Non-goals

Cursor ([#7342](https://github.com/bobmatnyc/trusty-tools/issues/7342), milestone #99). A change of the default runtime. Content rendering. Non-tmux sessions.

---

## 2. The runtime adapter bundle {#SPEC-RUNTIMES-02~draft}

**ID:** SPEC-RUNTIMES-02~draft
**Status:** Draft

### 2.1 Shape

The bundle **extends** `RuntimeAdapter`. The three existing methods stay with their signatures. The bundle adds a supertrait, `RuntimeControl: RuntimeAdapter`, holding the operations in §2.2. `build_adapter` returns the bundle. Names below are the contract's vocabulary; the implementing PR may adjust spelling but not behavior.

`RuntimeKind` gains `Codex`, `OpenCode` and `Gemini`, with wire strings `codex`, `opencode`, `gemini`, in the existing kebab-case serde form and `clap::ValueEnum` names (the pinning rule at [`runtime/mod.rs:191`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/mod.rs#L191)). `FromStr` keeps its current rule: an unknown value is a `RuntimeError`, never a default. `Default` stays `ClaudeCode`.

### 2.2 Operations

Required: all four runtimes can implement them.

| Operation | Contract |
|---|---|
| `launch(spec) -> Command` | argv and env for an interactive start in `spec.cwd`. `spec` carries model, permission intent, extra-context text, MCP servers, config home. |
| `launch_headless(spec, prompt) -> Command` | One-shot run with machine-readable output (`-p`, `exec --json`, `run --format json`). The launcher does not call it today; it is required so scripted use needs no second abstraction. |
| `map_permission(intent) -> Rendered` | `intent` is one of read-only, plan, edit, auto, bypass. `Rendered` holds flags or config text plus a `closest_match: bool`. It is `true` whenever the runtime has no exact equivalent. |
| `inject_context(text) -> Rendered` | A native flag where one exists (Claude Code). Otherwise a generated instruction file (`AGENTS.override.md`, `GEMINI.md`) or per-launch config. Never replaces the runtime's core system prompt. |
| `resume(selector) -> Command` | `selector` is `Id(String)` or `Last`. The launcher passes `Last` only when the caller names it explicitly; a managed-session resume always passes `Id`. |
| `config_home(spec) -> Env` | Env that isolates the runtime's config directory for this launch. |
| `mcp_config(servers) -> Rendered` | Servers rendered into the runtime's format: JSON `mcpServers`, TOML `mcp_servers`, or JSON `mcp`. |
| `pane_cues() -> PaneCues` | `is_idle(capture)`, `classify_input(capture)`, `blocking_modal_markers()`. Per-runtime. Only Claude Code's are known today. |
| `send_text(pane, text)` | Typed text or slash command into the pane. The tmux transport is shared and runtime-neutral. |
| `identify() -> {name, version, installed}` | A `--version` probe. A failed probe is an error (§5.4). |
| `install_assets(project)` | Calls the ADR-0059 content adapter for this host layout. |

Optional capabilities. The adapter reports each as supported, degraded or absent; the launcher degrades when absent.

| Capability | Meaning |
|---|---|
| `hooks` | Register lifecycle callbacks (session start, pre/post tool, turn end). Config file for Claude Code, Codex and Gemini; JS plugin for OpenCode. Fallback: pane polling. |
| `status_line` | Render a status line from runtime payloads. |
| `session_locator` | Where the conversation is stored: JSONL (Claude Code, Codex), SQLite (OpenCode), chat directory (Gemini). |
| `choose_session_id` | Caller picks the id at launch. Claude Code only (`--session-id`). |
| `append_system_prompt` | Native append. Claude Code only. |
| `plan_mode` | Native plan mode. Claude Code, Gemini, OpenCode (as an agent). Not Codex. |
| `server_mode` | Claude Code `remote-control`, Codex `app-server`, OpenCode `serve`. None found for Gemini. |
| `guard_coverage` | Whether `pm-guard` can run. See §2.4. |

### 2.3 Claude Code controls mapped to operations

Every control the launcher uses today. `Source` links to the line. The last column is the operation that owns the control after step 2 of §4.

| Control | Current location | Adapter operation |
|---|---|---|
| `claude` on the pane line | [`core/model_inject.rs:326`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L326), [`:390`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L390) | `launch` |
| `--model` | [`core/model_inject.rs:434`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L434) | `launch` (spec.model) |
| Other pane-line builders: in-place, client, agent | [`core/model_inject.rs:496`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L496), [`:516`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L516), [`:571`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L571), [`:601`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L601) | `launch`, `launch_headless` for the agent builder |
| `--setting-sources project,local` | [`core/model_inject.rs:735`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L735) | `launch` (Claude-only argv) |
| `--mcp-config` | [`core/session_mcp_scope.rs:273`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/session_mcp_scope.rs#L273) | `mcp_config` |
| `--append-system-prompt-file` | [`core/standalone/run.rs:144`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/standalone/run.rs#L144) | `inject_context` |
| `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN`, `CLAUDE_CODE_DISABLE_MOUSE` | [`core/alt_screen.rs:73`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/alt_screen.rs#L73), [`:279`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/alt_screen.rs#L279) | `launch` (env) |
| `ANTHROPIC_API_KEY` scrub | [`core/claude_env_scrub.rs:239`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/claude_env_scrub.rs#L239) | `launch` (env policy) |
| Keychain OAuth token keyed by the `CLAUDE_CONFIG_DIR` hash | [`core/oauth_token.rs:211`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/oauth_token.rs#L211), used at [`core/model_inject.rs:336`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L336) | `config_home` |
| Hook events (`PreToolUse`, `SessionStart`, …) | [`core/hook.rs:137`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/hook.rs#L137), [`core/standalone/hooks/mod.rs:264`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/standalone/hooks/mod.rs#L264) | `hooks` |
| `.claude/settings.json` writes | [`session_manager/provisioning_ledger.rs:34`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/provisioning_ledger.rs#L34), [`session_manager/decommission_force.rs:127`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/decommission_force.rs#L127) | `hooks`, `config_home` |
| `pm-guard` on the Claude hook protocol | `bin/tm/commands/pm_guard*.rs` | `guard_coverage` |
| Status line entry | [`core/statusline_settings.rs:105`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/statusline_settings.rs#L105), [`:273`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/statusline_settings.rs#L273), [`core/doctor_repair.rs:387`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/doctor_repair.rs#L387) | `status_line` |
| Permission mode: `--dangerously-skip-permissions` | [`core/model_inject.rs:264`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L264) | `map_permission` (`bypass`) |
| `permission_mode` in the hook payload | [`bin/tm/commands/hook_payload.rs:175`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/bin/tm/commands/hook_payload.rs#L175) (a payload fixture; no production reader found) | `hooks` payload parser |
| `--resume <id>` | [`runtime/claude_code.rs:481`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/claude_code.rs#L481), [`runtime/managed_launch.rs:281`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/managed_launch.rs#L281) | `resume` |
| Resume id existence check | [`runtime/claude_code.rs:215`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/claude_code.rs#L215) | `session_locator` |
| Transcripts under `~/.claude/projects` | [`core/project_discovery.rs:60`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/project_discovery.rs#L60), [`session_manager/worktree_claude_registry.rs:340`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/worktree_claude_registry.rs#L340) | `session_locator` |
| `CLAUDE_CONFIG_DIR` | [`core/paths.rs:695`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/paths.rs#L695), [`core/delegation_authority.rs:579`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/delegation_authority.rs#L579), [`core/home_write_fence.rs:93`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/home_write_fence.rs#L93), [`session_manager/worktree_claude_registry.rs:474`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/worktree_claude_registry.rs#L474) | `config_home` |
| Input box (`❯`, empty/suggestion/typed) | [`core/input_box.rs:18`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/input_box.rs#L18), [`:64`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/input_box.rs#L64) | `pane_cues.classify_input` |
| Trust and bypass dialogs | [`session_manager/task_inject.rs:99`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/task_inject.rs#L99), [`:101`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/task_inject.rs#L101) | `pane_cues.blocking_modal_markers` |
| Injection gated on `ClaudeCode` | [`session_manager/task_inject.rs:186`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/task_inject.rs#L186), [`:461`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/task_inject.rs#L461) | `pane_cues` (the gate becomes a capability check) |
| Submit probe | [`session_manager/submit_probe.rs:178`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/submit_probe.rs#L178) | `pane_cues.is_idle` |
| Shell-prompt handshake (probe timing) | [`runtime/pane_handshake.rs:45`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/pane_handshake.rs#L45), [`:60`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/pane_handshake.rs#L60) | stays runtime-neutral: it checks the shell, not the runtime |
| tmux send path | [`daemon/services/tmux_service.rs:158`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/daemon/services/tmux_service.rs#L158) | `send_text` (transport stays shared) |
| Agents in `~/.claude/agents` | [`core/paths.rs:54`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/paths.rs#L54), `core/agent_deployer.rs` | `install_assets` |
| Skills in `.claude/skills` | [`core/stale_skills.rs:237`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/stale_skills.rs#L237) | `install_assets` |
| `CLAUDE.md` seed, writer, sections | `core/claude_md_{seed,writer,sections}.rs` | `install_assets` and `inject_context` |
| `runtime` default | [`session_manager/manager.rs:405`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/manager.rs#L405), [`session_manager/supervisor_register.rs:292`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/supervisor_register.rs#L292) | `RuntimeKind::default()` (unchanged) |
| CLI `--runtime` flag, default `claude-code` | [`bin/tm/cli/mod.rs:1011`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/bin/tm/cli/mod.rs#L1011), [`bin/tm/cli/actions/session.rs:175`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/bin/tm/cli/actions/session.rs#L175), [`bin/tm/cli/actions/watch.rs:74`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/bin/tm/cli/actions/watch.rs#L74) | `RuntimeKind` `ValueEnum` (gains variants) |
| HTTP `runtime` field | [`core/sm/control.rs:292`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/sm/control.rs#L292) | `RuntimeKind::from_str` |
| `/compact` in injected prose | [`core/session_launch/settings.rs:57`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/session_launch/settings.rs#L57) | `send_text` or removal per runtime; prose only, not hardcoded as pane input |

### 2.4 Guard coverage

`pm-guard` runs as a Claude Code `PreToolUse` hook. A runtime without an equivalent hook runs unguarded. Required behavior:

- The adapter reports `guard_coverage` as `full`, `partial` or `none`.
- `tm sessions ls` and the spawn response show it for every non-Claude session.
- Codex and Gemini document pre-tool hooks (`PreToolUse`; `BeforeTool`). Whether the `pm-guard` payload parser accepts their stdin JSON is not established. OpenCode offers `tool.execute.before` through a plugin only. Each adapter issue states the coverage it ships.

---

## 3. Per-runtime capability gaps {#SPEC-RUNTIMES-03~draft}

**ID:** SPEC-RUNTIMES-03~draft
**Status:** Draft

Source: the 12-control survey of 2026-10-05 (Annex A). Versions: Codex rust-v0.160.0 (Apache-2.0), OpenCode v1.18.34 (MIT), Gemini CLI v0.62.0 (Apache-2.0). Claude Code npm 2.1.289.

### 3.1 Matrix

| # | Control | Claude Code | Codex | OpenCode | Gemini CLI |
|---|---|---|---|---|---|
| 1 | Launch, model, system prompt, cwd | `--model`, `--append-system-prompt[-file]`, cwd = process cwd | `-m`, `-C`, `-c k=v`, `-p profile`; replace via `model_instructions_file`; append UNCONFIRMED | `opencode`, `run`; `-m provider/model`; prompt via agent `prompt` field; cwd flag UNCONFIRMED (use process cwd) | `gemini`, `-p`, `-i`; `-m`; replace only via `GEMINI_SYSTEM_MD`, no append |
| 2 | Permission modes | default, acceptEdits, plan, auto, dontAsk, bypassPermissions | `-s read-only\|workspace-write\|danger-full-access`, `-a on-request\|never`, dangerous-bypass flag; no plan mode found | per-tool `allow\|ask\|deny` in config; built-in `plan` agent; CLI mode flag UNCONFIRMED | `--approval-mode default\|auto_edit\|yolo\|plan`; `-s` sandbox |
| 3 | Resume, storage, ids | `-c`, `-r`, `--session-id`, JSONL under `~/.claude/projects/` | `codex resume [--last\|<id>]`; JSONL under `~/.codex/sessions/`; caller-chosen id UNCONFIRMED | `run -c`, `run -s <id>`; SQLite `opencode.db`; caller-chosen id UNCONFIRMED | `--resume latest\|N\|<uuid>`; chats under `~/.gemini/tmp/<project_hash>/`; format and caller-chosen id UNCONFIRMED |
| 4 | Hooks | `PreToolUse`, `PostToolUse`, `Stop`, `SessionStart`, … | `PreToolUse`, `PermissionRequest`, `PostToolUse`, `Stop`, `SessionStart/End`, …; non-managed hooks need `/hooks` trust review | none as a file; JS/TS plugins with 30+ events incl. `session.idle`, `tool.execute.before/after` | 10 events incl. `BeforeTool`, `AfterAgent`; no `Stop` or `UserPromptSubmit` by name; pure JSON on stdout |
| 5 | Slash and custom commands | built-ins, `.claude/commands/*.md`; typing into the pane works | built-ins; custom prompts deprecated for skills; typing into the pane works (assumed, UNCONFIRMED) | built-ins; `.opencode/commands/*.md`; typing works (assumed) | `.gemini/commands/*.toml`; `/resume`, `/memory` |
| 6 | Instruction files | `CLAUDE.md`, output styles | `AGENTS.md`, `AGENTS.override.md`; no output styles found | `AGENTS.md`, `CLAUDE.md` fallback; no output styles | `GEMINI.md`, `@file` imports; no output styles found |
| 7 | MCP client | `--mcp-config`, `.mcp.json` | `[mcp_servers.<id>]` in `config.toml` | `"mcp"` in `opencode.json` | `"mcpServers"` in `settings.json` |
| 8 | Subagents, skills | `.claude/agents/*.md`, skills | `agents.*` settings, `.agents/skills` | `.opencode/agents/*.md`; reads `.claude/skills` | `.gemini/agents/*.md` |
| 9 | Pane cues | known to trusty-mpm | UNCONFIRMED (configurable footer `tui.status_line`) | UNCONFIRMED (`tui.json`; `/` and `!` prefixes) | UNCONFIRMED (footer shows context-file count) |
| 10 | Config dir env | `CLAUDE_CONFIG_DIR` | `CODEX_HOME`; project `.codex/config.toml` only when trusted | `OPENCODE_CONFIG_DIR`, `OPENCODE_CONFIG`, `OPENCODE_CONFIG_CONTENT`, `OPENCODE_DB` | `GEMINI_CLI_HOME` |
| 11 | Headless, JSON, server | `-p`, `--output-format json`, `remote-control` | `exec --json`, `app-server` | `run --format json`, `serve` (HTTP, SSE, `127.0.0.1:4096`) | `-p`, `--output-format json` |
| 12 | Auth | subscription or API key | ChatGPT login or API key | API keys plus OAuth | Google OAuth, API key, Vertex |

### 3.2 Codex

Gaps and degradations:

1. No append-system-prompt flag; replace only through `model_instructions_file`. Degrade: write launcher context to a generated `AGENTS.override.md`, or inject it as the first prompt. Never replace the model instructions.
2. The caller cannot choose the session id at launch (UNCONFIRMED). Degrade: read the id from the first `SessionStart` hook payload (`session_id` on stdin), else from `~/.codex/session_index.jsonl` or the newest rollout file.
3. No plan or acceptEdits mode, only sandbox × approval pairs. Degrade: acceptEdits → `workspace-write` + `on-request`; plan → `read-only` + `on-request`; bypass → the dangerous flag. `closest_match` is `true` for the first two.

Also: hooks outside the managed set need `/hooks` trust review before they run, so a launcher-written hook can sit inactive. The adapter detects and reports that state.

### 3.3 OpenCode

1. No hook file and no Stop event; only in-process JS plugins. Degrade: ship a launcher-owned plugin that POSTs `session.idle` and `tool.execute.*` to the tm daemon; fall back to pane polling.
2. No permission-mode CLI flag found (UNCONFIRMED), and sessions live in SQLite, not a transcript file. Degrade: generate a per-launch config through `OPENCODE_CONFIG_CONTENT` with `permission` set; read sessions through the `serve` HTTP API or `opencode export`.
3. No system-prompt append and no caller-chosen session id (UNCONFIRMED). Degrade: write a per-launch agent markdown (`prompt`) in `OPENCODE_CONFIG_DIR`; discover the id with a `GET` on the server.

Also: `serve` defaults to `127.0.0.1:4096`. Concurrent sessions need distinct ports, loopback only (ADR-0018), and `OPENCODE_SERVER_PASSWORD`.

### 3.4 Gemini CLI

1. System prompt is replace-only (`GEMINI_SYSTEM_MD`). Degrade: write a launcher `GEMINI.md` or `.gemini/` context file. Never set `GEMINI_SYSTEM_MD`.
2. No `Stop` or `UserPromptSubmit` hooks by those names, and hooks must emit pure JSON on stdout. Degrade: `AfterAgent` as the Stop analogue, `BeforeAgent` as prompt-submit; poll the pane for idle.
3. Resume is by index or UUID with project-hash storage; no caller-chosen id (UNCONFIRMED). Degrade: capture the UUID from the `SessionStart` hook or `--list-sessions`; isolate with `GEMINI_CLI_HOME` per launch.

### 3.5 Pane cues, all three runtimes: UNCONFIRMED

Idle-prompt and input-box cues are UNCONFIRMED for Codex, OpenCode and Gemini CLI. Docs cannot answer them. Item E (§6) captures them with `tmux capture-pane` for idle, typed draft, suggestion, busy, and each startup dialog. An adapter's `pane_cues` returns "unknown" until E lands for that runtime; the launcher then refuses to inject and says why (§5.4).

---

## 4. Migration of the Claude-only paths {#SPEC-RUNTIMES-04~draft}

**ID:** SPEC-RUNTIMES-04~draft
**Status:** Draft

Rule: Claude Code behavior never changes. Extract behind the adapter first, with golden tests proving identical output; add runtimes only after.

| Step | Change | Modules |
|---|---|---|
| 0 | Capture goldens from `de478389ef`: every Claude pane-line builder, env set, settings write and resume argv. Commit them before any move. | `core/model_inject.rs`, `core/alt_screen.rs`, `core/claude_env_scrub.rs`, `core/session_launch/settings.rs`, `runtime/claude_code.rs`, `runtime/managed_launch.rs` |
| 1 | Add `RuntimeControl`, the operation types and a fake adapter. No call site changes. | `runtime/mod.rs` and a new `runtime/control.rs` |
| 2a | Move launch rendering behind `launch`, `map_permission`, `inject_context`, `mcp_config`, `config_home`. Move the Claude-named modules under the Claude adapter. | `core/model_inject.rs`, `core/session_mcp_scope.rs`, `core/standalone/run.rs`, `core/alt_screen.rs`, `core/claude_env_scrub.rs`, `core/oauth_token.rs` |
| 2b | Move resume and transcript lookup behind `resume` and `session_locator`. | `runtime/managed_launch.rs`, `runtime/claude_code.rs`, `core/project_discovery.rs`, `session_manager/worktree_claude_registry.rs`, `core/auto_resume.rs` |
| 2c | Move hooks, settings and status line behind `hooks`, `status_line`, `guard_coverage`. | `core/hook.rs`, `core/standalone/hooks/`, `core/statusline_settings.rs`, `core/session_launch/settings.rs`, `session_manager/provisioning_ledger.rs`, `session_manager/decommission_force.rs` |
| 2d | Move pane parsing behind `pane_cues`. Replace the `RuntimeKind::ClaudeCode` gates with capability checks. | `core/input_box.rs`, `session_manager/task_inject.rs`, `session_manager/submit_probe.rs` |
| 2e | Move asset deployment behind `install_assets`. | `core/agent_deployer.rs`, `core/agent_skill_codeploy.rs`, `core/bundled_roster.rs`, `core/stale_skills.rs`, `core/claude_md_{seed,writer,sections}.rs` |
| 3 | Add `RuntimeKind` variants and CLI/HTTP values, one runtime at a time: Codex, OpenCode, Gemini. Each lands only after its pane-cue capture (item E). | `runtime/mod.rs`, `bin/tm/cli/`, `core/sm/control.rs` |

Constraints:

- `RuntimeKind::default()`, the `--runtime` defaults and the `claude`/`claude_code` aliases do not change.
- `tcode` is untouched. It implements the bundle with the operations it already has and reports the rest as absent.
- Step 2 ships as small PRs, each with the golden diff empty. A non-empty golden diff blocks the PR.
- The persisted `runtime` field gains new wire strings only in step 3. Rollback after step 3: a record with a new variant fails to parse in an older `tm`. The spawn route must refuse to launch a new variant until the daemon version supports it, and `tm` must name the version in the error.

---

## 5. Test strategy {#SPEC-RUNTIMES-05~draft}

**ID:** SPEC-RUNTIMES-05~draft
**Status:** Draft

### 5.1 Unit and golden tests, per adapter

- argv and env rendering for each operation: a table test per adapter, one row per permission intent, with `closest_match` asserted.
- Golden files for full launch commands. The Claude goldens are captured at step 0 from `de478389ef`. Codex, OpenCode and Gemini goldens are written with their adapters and reviewed by hand against Annex A.
- Rendered config (TOML, JSON, agent markdown) is parsed back and asserted structurally, not by string match alone.

### 5.2 Fake runtime for daemon tests

A fake adapter implements the bundle with a scripted pane (a shell script that prints a chosen idle prompt and modal strings). Daemon tests drive spawn, list, resume, idle detection and injection against it through the shared tmux driver. The fake prints no Claude Code strings, so any leftover Claude assumption in a launcher path fails the test.

### 5.3 Claude Code regression

- The step 0 goldens run on every PR touching `runtime/` or the moved modules.
- The existing `trusty-mpm` suites run unmodified. A moved test keeps its assertions.
- A test asserts `RuntimeKind::default()`, the CLI default and the HTTP default each remain `claude-code`.

### 5.4 No silent default (the fail-open rule)

The brief names this the fail-open rule. The behavior is an error:

- An unknown runtime string errors in `FromStr`, as today ([`runtime/mod.rs:237`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/mod.rs#L237)).
- A persisted record whose runtime this binary does not know errors on resume. It does not fall back to `claude-code`.
- A failed `identify()` probe (binary missing, `--version` fails) errors at spawn with the binary name. It does not launch Claude Code instead.
- An adapter whose `pane_cues` are unknown refuses to inject and reports why.

Each bullet has a test that fails if the code falls back to Claude Code.

### 5.5 Live smoke runs

One smoke run per runtime: spawn, `tm sessions ls`, inject a task into an idle pane, kill the pane, resume. Each is gated on the binary being installed (`which codex` etc.); absent, the test reports skipped, not passed. Smoke runs use `scripts/sandbox_daemon.sh` (CLAUDE.md, #9121). Interactive login is an operator step; a smoke run never logs in.

### 5.6 Test-ladder rung per phase

| Phase | Rung | Why |
|---|---|---|
| E: pane-cue capture spike | 2 if it commits fixtures; 1 if it commits only docs | Fixtures and a classifier test; no behavior change |
| A: adapter bundle extraction | 5 | Process lifecycle and persistence; `code-critic` round; `--include-ignored` |
| B, C, D: a runtime adapter | 6 | New `--runtime` values on the CLI and HTTP surface, plus a persisted enum variant; one binary smoke run each |

---

## 6. Sub-issue breakdown {#SPEC-RUNTIMES-06~draft}

**ID:** SPEC-RUNTIMES-06~draft
**Status:** Draft

Epic: "trusty-mpm multi-runtime launcher", milestone `trusty-mpm 2.3.0 · runtimes` (due 2026-12-18, after 2.2.0). Related, not children: #7342, #5418, #5421, #7338, #8619.

Order: A and E run first and in parallel. B follows A and E. C follows B. D follows C. The order is by popularity and tracker readiness (PRD ranking).

Each acceptance criterion below fails on a wrong implementation; the failure it catches follows the criterion.

### A. Adapter bundle: extract every Claude Code control

Scope: §4 steps 0–2.

- A1. Every row of the §2.3 table has its operation, and the old call site calls the operation. *A partial extraction leaves a direct `claude` string at a call site; the fake-runtime test (§5.2) sees Claude markers in a fake pane and fails.*
- A2. All Claude goldens from `de478389ef` match byte for byte. *A reordered flag or a changed env var fails the golden.*
- A3. The existing `trusty-mpm` suites pass with no assertion edited. *An extraction that changes behavior fails an old test.*
- A4. `RuntimeKind::default()`, the CLI default and the HTTP default are `claude-code`. *Defaulting to the fake or to `None` fails.*
- A5. The fake adapter drives spawn, list, resume, idle and inject in a daemon test with no Claude Code string in the pane. *A hardcoded `ClaudeCode` gate at `task_inject.rs:186` or `:461` makes injection never fire; the test fails.*
- A6. An unknown runtime string, an unknown persisted runtime and a failed `identify()` each error (§5.4). *A `unwrap_or_default()` passes the happy path and fails these three.*
- A7. `guard_coverage` is reported for `tcode` and Claude Code. *Omitting it fails the `tm sessions ls` assertion.*

### B. Codex adapter (absorbs #7343)

Scope: `RuntimeKind::Codex`; §3.2. Closes #7343 as superseded when it merges.

- B1. `tm session new --runtime codex` starts `codex` in the managed cwd; `tm sessions ls` shows `codex` and live. *A pane running a shell, not codex, fails the live check.*
- B2. Permission rendering: acceptEdits → `-s workspace-write -a on-request`; plan → `-s read-only -a on-request`; bypass → `--dangerously-bypass-approvals-and-sandbox`; `closest_match` is `true` for the first two. *Mapping plan to `danger-full-access` fails the table test.*
- B3. Context is injected through a generated `AGENTS.override.md`. `model_instructions_file` is never set. *Setting it replaces Codex's instructions; the argv golden rejects it.*
- B4. Resume passes the captured session id (`codex resume <id>`). When no id is known it errors and never passes `--last`. *Passing `--last` resumes a different conversation (the #6765 failure for Claude Code); the test with two stored rollouts fails.*
- B5. `CODEX_HOME` is set per launch; `~/.codex` is byte-identical before and after a smoke run. *Writing hooks into `~/.codex` fails the hash check.*
- B6. If launcher-written hooks await `/hooks` trust review, the launcher reports `hooks inactive`. *Silent success with dead hooks fails.*
- B7. Idle detection and injection use fixtures from item E for Codex. *Guessing a prompt glyph without a capture fails the fixture-provenance test: each cue regex names its fixture and Codex version.*
- B8. `guard_coverage` is stated, with a test that feeds a Codex `PreToolUse` stdin payload to `pm-guard` and records accept or reject.

### C. OpenCode adapter

Scope: §3.3.

- C1. Spawn, list and resume work as in B1 and B4 for `--runtime opencode`.
- C2. Permissions render into `OPENCODE_CONFIG_CONTENT`, never into the operator's `opencode.json`. *Editing the user file fails the hash check.*
- C3. The launcher-owned plugin posts `session.idle` to the daemon; a test with a scripted plugin event marks the session idle without any pane capture. *A pane-poll-only implementation fails.*
- C4. `serve` binds loopback only, on a port unique per session, with `OPENCODE_SERVER_PASSWORD` set. *Two concurrent sessions on `4096`, or a `0.0.0.0` bind, fail.*
- C5. Session discovery goes through the server API or `opencode export`; no SQLite file is opened directly unless the adapter pins `OPENCODE_DB` to its own path.
- C6. Pane cues come from item E fixtures, version-stamped (as B7).
- C7. `guard_coverage` is stated, with the plugin's `tool.execute.before` coverage tested or reported as `none`.

### D. Gemini CLI adapter (last)

Scope: §3.4. Starts after C merges.

- D1. Spawn, list and resume work for `--runtime gemini`.
- D2. `GEMINI_SYSTEM_MD` is never set; context goes into a generated `GEMINI.md`. *Setting it replaces the core prompt; the env golden rejects it.*
- D3. `GEMINI_CLI_HOME` is set per launch; `~/.gemini` is byte-identical before and after a smoke run.
- D4. Hooks write pure JSON to stdout. *A hook that prints a banner breaks Gemini's parser; a test runs each hook and parses stdout as one JSON value.*
- D5. `AfterAgent` maps to the Stop analogue and `BeforeAgent` to prompt-submit, with a test per mapping.
- D6. The session UUID is captured from `SessionStart` or `--list-sessions`; with none, resume errors (no `latest`). *Resuming `latest` can pick another project's chat.*
- D7. Pane cues come from item E fixtures, version-stamped.

### E. Empirical pane-cue capture spike (separate issue)

Judged separate: B, C and D all depend on it, and it needs the three binaries installed and logged in, which is an operator step.

- E1. For each runtime, `tmux capture-pane -e -p` fixtures for: fresh idle, typed draft, suggestion (if any), busy, each startup or trust dialog, a permission prompt. Each fixture records the runtime version and capture date.
- E2. For each cue, the record states UNCONFIRMED → confirmed, or stays UNCONFIRMED with the reason. *A fixture without a version stamp fails review.*
- E3. A classifier (`is_idle`, `classify_input`, `blocking_modal_markers`) per runtime passes its own fixtures and rejects the other runtimes' idle fixtures. *A regex that matches everything passes its own fixtures but accepts the others'; the cross-runtime test fails it.*
- E4. Confirms or refutes the "typing into the pane works" assumption (matrix row 5) for Codex and OpenCode.

---

## Annex A. Sources {#annex-a-sources}

Read 2026-10-05.

- Claude Code: [CLI reference](https://code.claude.com/docs/en/cli-reference), [hooks](https://code.claude.com/docs/en/hooks), [sessions](https://code.claude.com/docs/en/sessions), [permission modes](https://code.claude.com/docs/en/permission-modes), [npm](https://registry.npmjs.org/@anthropic-ai/claude-code).
- Codex: [developer commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli), [config reference](https://learn.chatgpt.com/docs/config-file/config-reference), [hooks](https://learn.chatgpt.com/docs/hooks), [AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md), [repo](https://github.com/openai/codex), [latest release](https://api.github.com/repos/openai/codex/releases/latest), [custom prompts](https://developers.openai.com/codex/custom-prompts), [session archiving (third party)](https://codex.danielvaughan.com/2026/06/02/codex-cli-session-archiving-lifecycle-management-v0136/).
- OpenCode: [CLI](https://opencode.ai/docs/cli/), [config](https://opencode.ai/docs/config/), [permissions](https://opencode.ai/docs/permissions/), [plugins](https://opencode.ai/docs/plugins/), [server](https://opencode.ai/docs/server/), [agents](https://opencode.ai/docs/agents/), [commands](https://opencode.ai/docs/commands/), [rules](https://opencode.ai/docs/rules/), [MCP](https://opencode.ai/docs/mcp-servers/), [skills](https://opencode.ai/docs/skills/), [TUI](https://opencode.ai/docs/tui/), [providers](https://opencode.ai/docs/providers/), [repo](https://github.com/anomalyco/opencode), [SQLite history (third party)](https://jazzyalex.github.io/agent-sessions/guides/opencode-sqlite-history.html).
- Gemini CLI: [CLI reference](https://geminicli.com/docs/cli/cli-reference/), [hooks](https://geminicli.com/docs/hooks/), [sessions](https://geminicli.com/docs/cli/session-management/), [settings](https://geminicli.com/docs/cli/settings/), [system prompt](https://geminicli.com/docs/cli/system-prompt/), [custom commands](https://geminicli.com/docs/cli/custom-commands/), [subagents](https://geminicli.com/docs/core/subagents/), [MCP](https://geminicli.com/docs/tools/mcp-server/), [headless](https://geminicli.com/docs/cli/headless/), [GEMINI.md](https://geminicli.com/docs/cli/gemini-md/), [repo](https://github.com/google-gemini/gemini-cli), [`GEMINI_CLI_HOME` issue (via search summary)](https://github.com/google-gemini/gemini-cli/issues/2815).
- The Claude Code column was verified for flags only. Claude Code's license is not confirmed here.
