---
spec_refs:
  - id: SPEC-TCMVP-03~draft
    path: docs/specs/DOC-75-tcode-tui-mvp-parity.md
    anchor: SPEC-TCMVP-03~draft
---

# DOC-76 — tcode TUI parity map: zones, messages, interaction, instruction adherence

**Status:** Draft
**Spec ID:** `SPEC-TCPARITY-01~draft` … `SPEC-TCPARITY-06~draft` (DOC-76)
**Subsystem:** `trusty-code` — `tcode tui`, `trusty-code-tui` shared REPL crate, `tui_client::engine`, `task::executor` tool registry; comparison baseline is `trusty-mpm`'s instruction-assembly and delegation stack
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-09-16
**DOC-N claim:** `DOC-76`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md). Verified free: `docs/specs/README.md`'s catalog note ("Next free `DOC-N` = `DOC-76`", recorded 2026-09-16) is current — the only hit for `DOC-76` under `docs/specs/**` was that note itself, and no open pull request claims it.
**Builds on:** [DOC-75](./DOC-75-tcode-tui-mvp-parity.md) §3 ([`SPEC-TCMVP-03~draft`](./DOC-75-tcode-tui-mvp-parity.md#SPEC-TCMVP-03~draft)) — the parity-target summary this document details zone by zone, message kind by message kind. Epic [#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939) — the tracking issue both documents serve.
**Milestone:** [trusty-code MVP · Claude Code TUI parity](https://github.com/bobmatnyc/trusty-tools/milestone/87)

---

Source: epic [#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939) comments
(2026-09-16). MVP decision: interactive session runs the no-delegate solo
agent ([#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184), merged) instead of the delegating PM.

**Owner correction, 2026-09-16 (verbatim):** "you should map to trusty-mpm,
not vanilla claude code, for instruction adherence." Section 1 (terminal-UI
zones and key bindings) stays mapped against Claude Code's own UI — that
comparison is unaffected. Section 2 (instruction-adherence baseline) and
Section 3 (message handling) below compare `tcode tui` against a
**trusty-mpm-launched Claude Code session** (`tm session start`): the PM
instruction package, the delegate-by-default PM with agents/skills, `tm
session` lifecycle, `tm hook --pm-guard`, and the per-turn injected palace
context — not a bare `claude` invocation.

## 1. Functional zones {#SPEC-TCPARITY-01~draft}

| Zone | Claude Code (believed unless noted) | tcode today | tcode target |
|---|---|---|---|
| Header/splash | Ink-rendered ASCII mark + cwd/version on launch | One line, no project name: `[tcode] connected to tcode daemon at <socket>` (`crates/trusty-code/src/tui_client/engine.rs:214-234`). `banner_lines` exists in the shared crate (`crates/trusty-code-tui/src/widgets/banner.rs`) but tcode never populates `ReplApp::banner_art`/`banner_title` — grep for `banner_lines` under `crates/trusty-code/src` returns nothing | Splash names client/daemon version+build, the project binding or "projectless", and the active workstream; the connect line names the bound home directory (or "projectless") and the active workstream. The session id is internal — logged to the daemon log and session record for recovery, never displayed by default. Implicit binding requires the cwd's enclosing git repository (`find_git_root`); a non-repo cwd stays projectless; `--project <dir>` still binds any directory. ([#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164), status:coded — PR [#8230](https://github.com/bobmatnyc/trusty-tools/pull/8230) open, not merged) |
| Scrollback/transcript | Main scroll region, wraps, reflows on resize | `crate::widgets::scrollback` builds `ChatLine`s from `ReplApp.chat`; ratatui redraws full size on `Resize` (no cached dims) | unchanged, parity already close |
| Assistant message blocks | Markdown-rendered bubbles, streamed | `AssistantOutput{chunk,done,is_error}` appended to the in-progress bubble at `streaming_idx` (`app/reduce.rs`); no markdown rendering | not in milestone 87 scope |
| Tool-call cards | Collapsed one-liner, ctrl+o/ctrl+r expands, red on failure | Cards render (`crates/trusty-code-tui/src/widgets/tool_card.rs`); glyph yellow/green/red on pending/success/`failed`; Ctrl-o toggles the newest card (`app/reduce.rs:604`, "the free ctrl bindings were o/b/f/k/w/y and this is the one Claude Code's own TUI uses") | expand/collapse landed ([#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596), status:merged) — verify multi-card toggle, not just "newest" |
| Permission prompt | Modal: allow-once / allow-always(session) / deny, y/a/n | `widgets/permission_prompt.rs`: `y` allow-once, `a` allow-for-session, `n`/Esc deny ([#3422](https://github.com/bobmatnyc/trusty-tools/issues/3422), merged) | present |
| Input composer | Multi-line via `\`, history recall, paste-safe | Single-line `ReplApp::input_buf`; Up/Down walk `history` ([#8181](https://github.com/bobmatnyc/trusty-tools/issues/8181), merged); no documented multi-line/paste path | multi-line composer out of milestone 87 |
| Statusline | Persistent: mode (`⏵⏵ bypass permissions on (shift+tab to cycle)`), model, cwd, context/cost, `/tasks` hint | `StatuslineUpdate(Vec<StatuslineSegment>)` renders via `widgets/status_line.rs`, but nothing after the first prompt refreshes it — no permission-mode segment, no Shift+Tab cycle, no `/tasks` hint | Resolved 2026-09-16 (owner ruling; see DOC-75 §4 row 4): [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182)'s MVP scope is statusline + subagent panel + `/tasks` + Shift+Tab permission-mode cycle, all in MVP — not statusline alone |
| Subagent panel | Background-task list, `/tasks` opens it | Delegation events (`DelegationStarted`/`DelegationFinished`) render as inline chat-log lines only; no standing list widget | In MVP via [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) (resolved 2026-09-16; DOC-75 §5 no longer excludes it, see DOC-75 §4 row 4): `/tasks` opens the panel, arrow-nav, Enter opens transcript, Esc returns |
| Footer/workstream line | N/A (Claude Code has no workstream concept) | `/workstream` segment plus one appended segment naming `ReplApp::active_agent` while a delegation is open (`widgets/status_line.rs`) | unchanged |

## 2. Instruction adherence baseline: trusty-mpm {#SPEC-TCPARITY-02~draft}

| Axis | tm-launched Claude Code | tcode |
|---|---|---|
| System-prompt assembly | Compile-time section table (9 files under `assets/instructions/sections/`: identity, core, memory, search, workflow, agent-delegation, enforcement, non-overridable-rules, framework-guaranteed-conventions — `crates/trusty-mpm/src/core/instruction_pipeline.rs:96-119`) plus a schema-v2 manifest (`crates/trusty-mpm/src/assets/instructions/pm-instruction-package.json:1-64`, `blocks` array drives emission order) assembled by `build_instructions`/`build_instructions_with_init` (`instruction_pipeline.rs:842,862`); a generated agent roster is appended to the `agent-delegation` section at compose time (`agent-delegation.md:26-29`). Delivered to the real `claude` binary via `--append-system-prompt-file <path>` (`crates/trusty-mpm/src/runtime/claude_code.rs:280-295,866-915`). Sections total 651 lines (`sections/*.md`) before the roster/output-style layer. | Fixed 4-slot concatenation — `BASE_PREAMBLE` + agent-card prompt + `project_context` + fallback guidance, joined by the same `\n\n---\n\n` separator (`crates/trusty-code/src/prompt/assembler.rs:44-52,157-183`). `BASE_PREAMBLE` plus two gated sections live in one 11,950-byte file (`crates/trusty-code/src/prompt/preamble.rs`); the agent card is the whole per-agent prompt — `pm.md` is 27 lines, `engineer.md` 20 (`crates/trusty-code/src/assets/agents/pm.md`, `engineer.md`). No roster-generation step, no manifest schema, no section-tier system. |
| Project instructions reaching the model | `CLAUDE.md` is loaded NATIVELY by Claude Code (never copied into the composed prompt) except for explicit marker overrides: `<!-- TRUSTY-MPM: <TOKEN> START v=1 -->` … `END` pairs replace one named section (`IDENTITY`/`MEMORY`/`SEARCH`/`WORKFLOW`/`AGENT-DELEGATION`/`ENFORCEMENT`/`NON-OVERRIDABLE-RULES`/`FRAMEWORK-GUARANTEED-CONVENTIONS`; `CORE` is always declined) — `crates/trusty-mpm/src/core/instruction_pipeline.rs:592-620`. The delivered prompt (with applied/declined markers) is written per session to `.trusty-mpm/last-instructions.md` (`compiled_prompt_path`, `instruction_pipeline.rs:328-330`; `write_compiled_prompt_to`, `:384-390`). trusty-mpm seeds the stub once and never edits the file again (issue [#2170](https://github.com/bobmatnyc/trusty-tools/issues/2170), `instruction_pipeline.rs:591-593`). | `crates/trusty-code/src/project_context/mod.rs:1-53` reads `<root>/CLAUDE.md` then `<root>/.claude/CLAUDE.md` (first match wins), verbatim, capped at `MAX_CONTEXT_BYTES` = 16 KiB with a truncation note on overflow (`:20-27`). No marker/override syntax, no per-section tiers, no compiled-prompt artifact written back for inspection. Directory precedence for the plugin layer is separately `.trusty-code` → `.claude` → `.open-mpm` (`crates/trusty-code/src/plugins/mod.rs:80-81`). |
| Workflow rules present | Delegation-by-default routing table naming `subagent_type`s verbatim, default-to-delegation for ops/infra/build, and a fixed `research → engineer → local-ops → qa → documentation` pipeline on "just do it" (`crates/trusty-mpm/src/assets/instructions/sections/agent-delegation.md:1-13`). The test ladder, per-PR changelog fragment, and 4-label issue lifecycle (open→in-progress→coded→merged→tested→closed) are THIS project's own `CLAUDE.md` prose, not trusty-mpm code — they reach the model because trusty-mpm's marker mechanism leaves non-marked `CLAUDE.md` prose untouched and Claude Code loads it natively (`instruction_pipeline.rs:613-616`). | `verify_gate` is the one shipped workflow rule: `finish_task` is refused (recoverable retry, not a hard stop) only when the task/project-context text names a test command AND that command is detectable for the bound project AND no matching `bash` call ran ([#2279](https://github.com/bobmatnyc/trusty-tools/issues/2279)/[#8206](https://github.com/bobmatnyc/trusty-tools/issues/8206), `crates/trusty-code/src/verify_gate/mod.rs:1-38`). No rung system, no `--no-fail-fast` requirement, no changelog-fragment gate, no issue-label lifecycle: `grep -rl "changelog.d\|status:coded\|status:merged\|status:tested" crates/trusty-code/src` returns zero files. |
| Tool-permission enforcement | Claude Code's own native permission dialogs, PLUS `tm hook --pm-guard` as a `PreToolUse` mechanical backstop: a local, no-daemon-round-trip classifier that denies the PM's own direct source-code Edit/Write and forbidden Bash verbs, exempts native `Task`/`Agent` sub-agent dispatches via the payload's `agent_id`, and allows a per-turn file-change budget before hard-blocking ([#1977](https://github.com/bobmatnyc/trusty-tools/issues/1977)/[#2014](https://github.com/bobmatnyc/trusty-tools/issues/2014)/[#2918](https://github.com/bobmatnyc/trusty-tools/issues/2918), `crates/trusty-mpm/src/bin/tm/commands/pm_guard.rs:1-40`). | `PermissionGate`: evaluate → ask → await → timeout, driven by each agent card's `permissions:` block (e.g. `pm.md`'s `bash: ask`, `write_file: ask`) plus a legacy allowlist for cards with no block; `HARNESS_REGISTERED_TOOLS` skips the check entirely ([#7948](https://github.com/bobmatnyc/trusty-tools/issues/7948), `crates/trusty-code/src/permissions/gate.rs:1-14`). An unanswered `ask` fails closed after `DEFAULT_ASK_TIMEOUT_SECS` = 300s (`gate.rs:38-42`). The TUI's y/a/n modal ([#3422](https://github.com/bobmatnyc/trusty-tools/issues/3422)) answers this gate, not a native Claude Code dialog. |
| Sub-agent dispatch & reporting | Native Claude-Code `Agent` tool; every routable name is a deployed `subagent_type`, passed verbatim (`agent-delegation.md:5-7`); the PM declares `isolation: "worktree"` per writer, which is the only signal `tm hook --pm-guard` can see — an agent-created worktree is invisible to it and gets the next dispatch wrongly denied ([#5649](https://github.com/bobmatnyc/trusty-tools/issues/5649), `crates/trusty-agents-common/src/assets/agents/BASE-AGENT.md:109-113`). Reporting is a four-part handoff (agent, done, remaining, constraints) governed by BASE-AGENT prose, not a wire schema. | `delegate_to_agent` tool: `{agent_name, task}`, pre-flight-validated against the on-disk agent-config directory (unknown name lists available agents), with a `redelegation_hint` appended on `TurnCapExceeded`/`Timeout`/`Cancelled` so the next attempt reuses partial disk state (`crates/trusty-code/src/tools/delegate.rs:1-30`). Completion is a structured `finish_task` payload — `status` enum, `summary`, optional `changes`/`tests_run`/`tests_passed` (`crates/trusty-code/src/tools/finish_task.rs:8,51,107`) — but §2 below notes `trusty-code-tui` currently flattens it to plain text ([#8204](https://github.com/bobmatnyc/trusty-tools/issues/8204)). No worktree-per-writer isolation concept exists in `delegate.rs` or `finish_task.rs`. |

### Supporting agents

The solo-agent decision (§2 of DOC-75) governs the interactive default; a
2026-09-16 roster check confirms tcode also carries four non-interactive
supporting agents, each narrower than the delegate-first PM this document
compares against. `DEFAULT_AGENTS` (`crates/trusty-code/src/assets/mod.rs:289-403`)
includes three `EmbeddedAgent::Direct` cards that are tcode-native rewrites of
the shared roster's originals — `ticketing`, `version-control`,
`documentation` (`crates/trusty-code/src/assets/agents/*.md`) — plus
`research`, an `EmbeddedAgent::Composed` roster agent whose content still
resolves from the shared `base-research` extends source
(`crates/trusty-agents-common/src/assets/agents/research.md:6`,
`extends: base-research`). Each of the three tcode-native cards carries its
own `tcode_tools:` allowlist that narrows the registry `ProjectToolFactory::build`
(`crates/trusty-code/src/task/executor.rs:929-983`) constructs — e.g.
`ticketing.md`'s allowlist (`read_file, grep, glob, list_dir, bash,
search_code, use_skill, finish_task`) has no `write_file`/`edit`, matching its
card's "never edits source code" rule. None of the three allowlists names a
dedicated `gh` tool: `git` and `gh` both run through the shared `bash` tool,
not a purpose-built wrapper. Separately, [#8199](https://github.com/bobmatnyc/trusty-tools/issues/8199)
(open) tracks the roster frontmatter emitter dropping the `permissions:` key
(and any key outside its fixed list) on the `deploy` agent card. Tracking
issue for this subsection's roster check: [#8227](https://github.com/bobmatnyc/trusty-tools/issues/8227).

## 3. Message handling {#SPEC-TCPARITY-03~draft}

Claude Code: in-process SDK stream inside the Ink process; message kinds
(user, assistant text, thinking, tool_use, tool_result, permission request,
subagent start/stop) are native SDK events, reduced directly into Ink state.

trusty-mpm adds message kinds a bare Claude Code session never sees, since
they originate outside the SDK stream:

| trusty-mpm message kind | Source | tcode analogue |
|---|---|---|
| `PreToolUse`/hook-injected deny or budget message | `tm hook --pm-guard` prints a `permissionDecision: "deny"` JSON response Claude Code renders as a tool-call failure (`crates/trusty-mpm/src/bin/tm/commands/pm_guard.rs:18-20`) | Partial — `PermissionRequested`/`PermissionResolved` cover the interactive `ask` path (`crates/trusty-code-tui/src/event.rs`, §1 row "Permission prompt"), but there is no mechanical PreToolUse deny layer underneath it; a `HARNESS_REGISTERED_TOOLS` call bypasses the gate outright (`permissions/gate.rs:1-14`) |
| Per-turn injected palace context | `trusty-memory` registers a `UserPromptSubmit` hook (parsed from `TRUSTY_MEMORY_HOOKS`) that trusty-mpm merges into every managed session's settings, re-running on each prompt submit (`crates/trusty-mpm/src/core/session_launch/project_hooks.rs:39,111`) | None — `crates/trusty-code/src/plugins/mod.rs:91` lists `hooks`/`mcpServers` as manifest keys Phase 1 explicitly recognizes but does not implement; no per-turn context-injection surface exists in the TUI or daemon |
| Cross-session messages (e.g. `memory_send_message`-style inter-project notes, background-agent task notifications) | Delivered as tagged drawers/session state outside the `session.events` stream tcode's producer maps from (see the `forward_session_event` table below) | None found — no `ReplEvent` variant or daemon `Event` carries a cross-session or inter-project message; `event.rs`'s catch-all silently drops anything unrecognized |

tcode's own session-level stream: daemon `session.events` over UDS (JSON-RPC
framed, not SSE);
producer `crates/trusty-code/src/tui_client/session_events.rs`
(`forward_session_event`) maps daemon `Event` variants to the shared
`ReplEvent` enum (`crates/trusty-code-tui/src/event.rs`, 558 lines):

- `Message`/`AgentMessage`/`PmThinking` → `AssistantOutput{done:false}`
- `AgentMessageDelta{agent_id,turn_id}` → `AgentOutput` (keyed, [#7940](https://github.com/bobmatnyc/trusty-tools/issues/7940) — keeps concurrent agent streams from interleaving into one bubble)
- `ToolStarted` → `ToolInvocation{result:None}`; `ToolFinished`/`ToolError` → `ToolInvocation{result:Some,failed}` (`failed` is the backend's verdict, not text-sniffed)
- `PmDelegating`/`AgentSpawned` → `DelegationStarted`; `AgentDone`/`AgentFailed` → `DelegationFinished(DelegationOutcome)`
- `PermissionRequested`/`PermissionResolved` → matching `ReplEvent` halves ([#3422](https://github.com/bobmatnyc/trusty-tools/issues/3422))
- `SessionDone` → terminal `AssistantOutput{done:true, is_error: status=="failed"}`; `SessionCancelled` → `StatusMessage`
- Everything else (progress/telemetry) silently ignored, forward-compatible; `engine_tests::every_agent_attributed_event_maps_or_is_explicitly_ignored` pins the catch-all against new agent-attributed variants landing unclassified

Reduced into UI state by `crates/trusty-code-tui/src/app/reduce.rs::apply`
(free function matching `ReplEvent`, mirrors `crate::run::event_loop`'s
`FnMut(&mut M, ReplEvent)` contract).

Dropped/flattened today: `finish_task`'s structured completion payload
(`status`, `summary`, `changes: Vec<ChangeEntry>`, `tests_run`,
`tests_passed`, defined in `crates/trusty-code/src/tools/finish_task.rs`)
is flattened by `render_finish_summary` into one string before
`agent_loop::build_finish_output` even puts it on the wire — `trusty-code-tui`
has zero reference to `finish_task`, and `reduce.rs` renders it as plain
`AssistantOutput` text. Tracked as [#8204](https://github.com/bobmatnyc/trusty-tools/issues/8204) (parse into dedicated slots,
fall back to plain text on malformed/partial payload).

## 4. Interaction model {#SPEC-TCPARITY-04~draft}

| Binding | Claude Code (believed) | tcode |
|---|---|---|
| Enter | submit | submit, gated `!app.busy` (`app/reduce.rs:639`) |
| Ctrl-C | interrupt turn | `pending_cancel = true`, relayed to `cancel_session` |
| Ctrl-D (empty buffer) | quit | quit, guarded empty-buffer only (ported from tagent) |
| Up/Down | prompt history recall | history recall ([#8181](https://github.com/bobmatnyc/trusty-tools/issues/8181), merged) |
| Ctrl-a/e/u | line-start/end/clear | same (tagent line-editor port) |
| Ctrl-o | expand/expand tool output | toggles newest tool card ([#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596)) |
| Shift+Tab | cycle permission mode | not found in `reduce.rs`'s key match — no `KeyCode` arm reads a Shift-Tab-shaped input; likely unimplemented despite [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182)'s acceptance text |
| Esc | interrupt / close picker | closes/denies an open permission prompt only; no open-picker concept in tcode |
| `/` palette | command palette autocomplete | no palette — `commands.rs::route` matches full typed slash lines only |

Slash commands. Claude Code: broad built-in set (`/model`, `/resume`,
`/clear`, `/help`, background-task commands, etc.). tcode today
(`crates/trusty-code-tui/src/commands.rs`): `/help`, `/clear`, `/quit`
(alias `/exit`), and one engine-routed command, `/workstream [list|activate
<id>]`. `/model` is exercised in `commands/tests.rs` as a forwarded (not
built-in) command; no `/resume`, `/tasks`, or `/session` yet.

Permission flow: prompt → answer → daemon round trip. `PermissionRequested`
(daemon-suspended call, `request_id` correlation key) opens
`ReplApp::pending_permission`; y/a/n answers via
`TuiEngine::respond_permission`; a relay failure re-opens the same prompt
via `PermissionAnswerFailed` ([#3422](https://github.com/bobmatnyc/trusty-tools/issues/3422)). Persisting "allow for session" as a
standing mode toggle, and exposing/cycling it from the statusline, is
[#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184)/[#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) territory, not fully landed.

Cancel/interrupt: `Ctrl-C` sets `pending_cancel`; [#8207](https://github.com/bobmatnyc/trusty-tools/issues/8207) (open) — cancel
reports "cancelled" client-side while the daemon task keeps running, so the
next prompt can fail with JSON-RPC `-32003` (task still busy). This is a
correctness gap in the cancel round trip, not just UI.

Resume: [#8185](https://github.com/bobmatnyc/trusty-tools/issues/8185) (open) — `engine.rs::setup` unconditionally calls
`session.create`; no `session.resume`/transcript-restore path exists in
`tui_client/`. `tcode attach`'s relationship to TUI resume is explicitly
unresolved per the issue body.

Session/daemon lifecycle visibility: [#8203](https://github.com/bobmatnyc/trusty-tools/issues/8203) (open, bug) — client should
refuse or restart when the daemon's build differs from the client's; no
such check exists today. [#8209](https://github.com/bobmatnyc/trusty-tools/issues/8209) (heartbeat frames) and
[#8208](https://github.com/bobmatnyc/trusty-tools/issues/8208) (session management parity with `tm`) were retargeted to
`trusty-agents` and removed from the tcode MVP on 2026-09-16 owner
confirmation — do not treat either as tcode scope.

## 5. Gaps table {#SPEC-TCPARITY-05~draft}

| Gap | Milestone-87 issue | Notes |
|---|---|---|
| Solo agent default (no PM delegation) | [#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184) | status:merged |
| Read/search/edit tools reachable by default agent | [#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602) | closed |
| Startup identity/splash | [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164) | status:coded — PR [#8230](https://github.com/bobmatnyc/trusty-tools/pull/8230) open, not merged |
| Persistent statusline (project/model/session/turn) | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) | open; scope note below |
| Prompt history recall | [#8181](https://github.com/bobmatnyc/trusty-tools/issues/8181) | status:merged |
| Session resume | [#8185](https://github.com/bobmatnyc/trusty-tools/issues/8185) | open |
| Tool-card expand/collapse | [#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596) | status:merged |
| `finish_task` structured rendering | [#8204](https://github.com/bobmatnyc/trusty-tools/issues/8204) | open, added 2026-09-16 |
| Cancel leaves daemon task running (-32003 on next prompt) | [#8207](https://github.com/bobmatnyc/trusty-tools/issues/8207) | open, bug |
| Client/daemon build-mismatch guard | [#8203](https://github.com/bobmatnyc/trusty-tools/issues/8203) | open, bug |
| Shift+Tab permission-mode cycling + `⏵⏵`/`/tasks` statusline text | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) (acceptance text) | Resolved 2026-09-16 (owner ruling, DOC-75 §4 row 4): [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182)'s scope is statusline + subagent panel + `/tasks` + Shift+Tab permission-mode cycle, all in MVP |
| Subagent panel (arrow-nav, Enter opens transcript, Esc returns) | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) | Resolved 2026-09-16: in MVP via #8182 (DOC-75 §5 no longer excludes it; see DOC-75 §4 row 4) |
| Multi-line input / paste handling | [#8222](https://github.com/bobmatnyc/trusty-tools/issues/8222) | filed 2026-09-16; MVP-relevant if a user pastes a multi-line diff or writes a long prompt |
| `/` command palette / autocomplete | [#8223](https://github.com/bobmatnyc/trusty-tools/issues/8223) | filed 2026-09-16; lower priority, DOC-75 doesn't mention it |
| Markdown rendering in assistant bubbles | none found | explicitly out of DOC-75 scope (not listed even as a gap) — not flagging as MVP-relevant |

Instruction-adherence gaps relative to trusty-mpm (§2), unfiled per this
task's scope:

| Gap | MVP-relevant? | Reason |
|---|---|---|
| No `CLAUDE.md` marker-section override (`<!-- TRUSTY-MPM: … START/END -->`) | Post-MVP | tcode reads `CLAUDE.md` verbatim and capped (`project_context/mod.rs:20-27,44`); an override mechanism is a customization feature, not required for the TUI to reach parity on rendering/interaction |
| No per-project instruction package (schema-v2 manifest, section tiers, generated roster) | Post-MVP | tcode's 4-slot assembler (`assembler.rs:157-183`) is a design choice for a smaller, no-delegate solo agent ([#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184)); a manifest system only matters once delegation is in scope, which milestone 87 explicitly excludes |
| No skill-invocation surface in the TUI | Post-MVP | `use_skill` is a registered tool per agent card (`pm.md`'s `tcode_tools` list), but nothing in `trusty-code-tui` renders a skill-use event distinctly from a generic tool card — cosmetic given skills already execute; not blocking for milestone 87's rendering/interaction scope |
| No cross-session messaging | Post-MVP | Requires a delegating multi-agent topology tcode's solo-agent MVP ([#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184)) deliberately does not have; nothing to message between yet |
| No hook layer (`PreToolUse`/`UserPromptSubmit`) | MVP-relevant | The verify-before-finish gate (`verify_gate/mod.rs`) is doing hook-shaped enforcement work already (refusing `finish_task`); a user reading the TUI transcript has no way to see that a mechanical gate fired vs. the model's own text, which affects the transcript/message-handling parity this milestone is scoped to (§3) |
| No changelog-fragment / ticket-lifecycle workflow rules | Post-MVP | These reach a trusty-mpm session only via this project's own `CLAUDE.md` prose (§2 row 3); tcode's solo-agent MVP has no delegation pipeline for them to route through, so enforcing them has no effect until delegation returns |

## 6. Test surface {#SPEC-TCPARITY-06~draft}

tmux-drivable (send-keys/capture-pane, integration-level, exercises the
real render + key loop): splash/banner text, statusline segment text,
permission-prompt render and y/a/n key handling, tool-card collapse/expand
keystroke (Ctrl-o), slash-command routing end to end, prompt-history
Up/Down recall, Ctrl-C cancel round trip against a real daemon.

Reducer unit tests (state-transition correctness, no terminal needed):
`crates/trusty-code-tui/src/app/reduce/tests.rs` — every `ReplEvent` variant
`apply` changes state for (busy/streaming_idx transitions, `TurnFinished`
generation-matching, tool-card toggle, delegation open/close). `event.rs`'s
own `#[cfg(test)] mod tests` covers `ReplEvent`/`WorkstreamSummary`
construction and round-tripping, not reduction.

Existing named harnesses:
- `crates/trusty-code/tests/tui_client_engine.rs` — `CodeEngine`
  integration tests against a real/mock daemon (session lifecycle,
  workstream commands).
- `crates/trusty-code-tui/src/app/reduce/tests.rs` — reducer unit tests.
- `crates/trusty-code/tests/permission_prompt_e2e.rs` — end-to-end
  permission-request-to-answer flow.
- `crates/trusty-code/src/tui_client/session_events_forward_tests.rs` and
  the adjoining `session_events_tests.rs` (the latter is `engine_state.rs`'s
  reconnect suite, [#6637](https://github.com/bobmatnyc/trusty-tools/issues/6637), not `forward_session_event`'s own tests — the two
  filenames read as duplicates but are not) — `forward_session_event`
  mapping coverage, including the [#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596) tool-failure flag.
- `crates/trusty-code-tui/src/widgets/permission_prompt/tests.rs` and
  `crates/trusty-code-tui/src/widgets/tool_card/tests.rs` — widget-level
  render assertions (prompt height growth, collapsed-card one-line
  summary, delegated-card gutter).
- `crates/trusty-code-tui/src/commands/tests.rs` — slash-command routing.

Gaps to verify via tmux specifically when built: [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164) splash (visual, no
reducer state to unit-test alone), [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) statusline live-refresh and any
Shift+Tab binding, [#8185](https://github.com/bobmatnyc/trusty-tools/issues/8185) resume (needs a real daemon round trip across two
process launches), [#8207](https://github.com/bobmatnyc/trusty-tools/issues/8207) cancel (needs to watch the daemon-side task, not
just client state).
