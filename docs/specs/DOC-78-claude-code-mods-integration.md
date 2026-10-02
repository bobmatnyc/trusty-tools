# DOC-78 — Claude Code Mods in trusty-mpm: Observation, Interface, and Guard Hardening

**Status:** Draft (proposed; not scheduled)
**Spec ID:** `SPEC-MODS-01~draft` … `SPEC-MODS-06~draft` (DOC-78)
**Target release:** post-2.0.0. The architect (formerly supervisor) release is milestone `trusty-mpm 2.0.0` ([#97](https://github.com/bobmatnyc/trusty-tools/milestone/97)). The next roadmap section after it is `cross-harness` ([#99](https://github.com/bobmatnyc/trusty-tools/milestone/99), stage "later"). That milestone covers Codex and Cursor, not Claude Code features, so this work is proposed as its own post-2.0.0 item, sequenced alongside #99. No numbered milestone exists for it yet.
**Subsystem:** `trusty-mpm` (launch settings, `tm hook`, daemon ingest, `content/`); `trusty-console` (event bus consumer).
**Owner:** Bob Matsuoka
**Last-updated:** 2026-10-02
**DOC-N claim:** `DOC-78`, per `docs/specs/README.md` ("Next free `DOC-N` = `DOC-78`"). No tracked file and no open PR names `DOC-78` (checked 2026-10-02). Re-verify before filing, per [DOC-38 §4.1](./spec-linked-documentation.md).
**Claude Code floor:** v2.1.287 (mods on by default; this is also the local install).

---

## 1. Context — how trusty-mpm observes and steers Claude Code today

| Mechanism | What it does | Source |
|---|---|---|
| Lifecycle settings hooks | Six events (`PreToolUse`, `PostToolUse` async, `Stop`, `SubagentStop`, `SessionStart`, `SessionEnd`) run `tm hook`, which POSTs to the daemon. `SessionStart`/`SessionEnd` capture the Claude session UUID for `--resume`. | `crates/trusty-mpm/src/core/standalone/hooks/mod.rs:264-300` |
| `SubagentStop` retry spool | Only the stop POST is retried and parked on disk; it frees builder slots. | `bin/tm/commands/hook_post.rs`, `core/stop_spool.rs` |
| pm-guard | `PreToolUse`, `matcher: ""`, `<abs>/tm hook --pm-guard`, `timeout: 10`. Local classification, no daemon round-trip. Hard floor (#8878) runs ahead of both bypasses; Architect pane guard (#8902). | `core/session_launch/settings.rs:767-795`; `bin/tm/commands/pm_guard.rs`; `pm_guard_floor.rs` |
| Notification push | `Notification` → one JSON line in `<inbox>/events.jsonl` (#8392); the Architect poller reads `permission_prompt` / `agent_needs_input`. | `bin/tm/commands/hook_notify.rs` |
| Prompt-feedback hooks | `Stop` / `SubagentStop` capture critiques (#7688). | `core/session_launch/prompt_feedback_hooks.rs:48` |
| Output style + appended prompt | `outputStyle` written into project settings when `claude --version` ≥ 1.0.83, else injected; PM instructions via `--append-system-prompt-file`. | `core/output_style.rs:33,203-250`; `core/instruction_pipeline.rs:51,293` |
| `statusLine` | `tm statusline` renders branch, compaction, rate limits from the statusLine payload. | `core/session_launch/settings.rs:1130-1190`; `bin/tm/commands/statusline/usage.rs` |
| MCP server | `mcp__trusty-mpm__*` tools, including `hook_event` (ingest a hook event into daemon observability). | `crates/trusty-mpm/src/mcp/mod.rs:93,660` |
| tmux observation | Architect poller runs `tmux capture-pane -e -p -S -80` on every pane and classifies the input box as empty / suggestion / typed. Open bug #8407: capture-pane cannot tell a typed draft from a suggestion. | `python/trusty-architect/scripts/fleet-poll.py:129-131`; `scripts/input-state.py` |
| Pause/resume snapshot | Scrollback + cwd captured via `capture-pane` before stop. | `session_manager/snapshot.rs` |
| Build lease | `tm build-lease` slot pool; pm-guard wraps heavy builds (#8261). | `core/build_lease/`; `bin/tm/commands/pm_guard_build_lease.rs` |
| Architect fleet | `tm fleet init` launches the Architect and its poller (`tm-architect-poll`). | `bin/tm/commands/fleet/poller.rs`; skill `assets/skills/tm-supervisor-setup.md` |
| Plugin scoping | Default-deny `enabledPlugins` per project (#7422). | `core/session_plugin_scope.rs` |
| Console | `trusty-console` is the one event bus: harnesses push `HarnessEvent` frames over UDS (DOC-73). | `crates/trusty-console/src/event_bus/mod.rs` |

Paths without a crate prefix are under `crates/trusty-mpm/src/`.

## 2. What mods are

- **Version.** The changelog entry for v2.1.287 (2026-10-01) reads "Added Claude Mods: plugins may now modify deeper behavior". The overview reads "Mods require Claude Code v2.1.287 or later, and they're on by default". An early-access switch, `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS`, existed before and is ignored from v2.1.287. Local install: `2.1.287 (Claude Code)`.
- **Stability.** The docs carry no "experimental" or "preview" label. They state: "The events and methods can change between releases, so trust these files over any page". The reference is "as of v2.1.287". Anthropic can turn installed mods off remotely (`hooks modules are turned off in this process`). **Reading: generally available, but with no API-stability promise.**
- **Shape.** A plugin whose `hooks/hooks.json` has `"modules": ["./register.js"]`. The module exports `register(on, options)`. A hook is `on(event, matcher?, async ($, e, next) => …)`, a middleware chain: return `next(e)` to observe, `next({...e, …})` to rewrite, or a result without `next` to answer. JS/TS, no build step, no Node APIs; all I/O goes through `$`.
- **Events used here** (names quoted from the reference): `tool.call`, `tool.check`, `prompt.submit`, `prompt.edit`, `prompt.suggest`, `prompt.section`, `turn.start`, `turn.step`, `turn.complete`, `session.start`, `session.end`, `session.compact`, `session.receive`, `session.send`, `session.measure`, `agent.spawn`, `ui.render`, `plugin.register`, and `classic.<Event>` for every settings-hook event.
- **Interface.** Render sites `Pane`, `AbovePrompt` (the "band"), `Spinner`, `PromptHint`, and others. Calls: `$.ui.open`, `$.ui.toast` (4 s default), `$.ui.status` (one line under the prompt, prefixed `⚠ <mod>:`), `$.ui.log`, `$.ui.ask`.
- **API namespaces:** `$.ui`, `$.command`, `$.tool`, `$.agent`, `$.model`, `$.prompt`, `$.turn`, `$.session` (`usage()` returns context `percent` and `rateLimits`), `$.fs`, `$.store`, `$.state`, `$.clock`, `$.http`, `$.process`, `$.mcp`, `$.env`, `$.settings`.
- **Hot reload.** `claude --plugin-dir <dir>` reloads on save. A broken save keeps the previous version loaded. `/reload-plugins` reloads installed plugins. `CLAUDE_CODE_PLUGIN_DIRS` loads dirs where no flag can be passed.
- **Limits.** 10 s hook execution, excluding time in `next` or a mods API call. 1 s for a `.catch` handler. 1.5 s for all `session.end` hooks together. `$.process.run` defaults to 30 s, 10 min max.
- **Where it runs.** Hooks run in the terminal, Desktop Code tab, VS Code chat, `claude -p`, Agent SDK, and cloud sessions that carry the plugin. Drawing appears only in the terminal and the Desktop app. Desktop WSL sessions get neither.
- **Tooling.** `claude plugin validate [--strict] [--json]` prints `hooks:` / `calls:` lines. `claude plugin test` runs `*.test.ts`. Per-version `.d.ts` files are written to `.claude-plugin/types/` on load.
- **Local corroboration.** The `plugin-authoring` skill is bundled inside the binary as built-in `cc-plugin-plugin-authoring`. No `SKILL.md` exists on disk under `~/.claude` or the tm config dir's plugin cache. The binary's description string matches the brief: "Make a mod: a live pane, band, status line, toast or hook inside Claude Code (terminal or desktop Code tab), written as a plugin of function hooks that hot-reloads in this session". The web docs say Claude writes such mods to `~/.claude/dev-mods/<session-id>/`, and the binary contains `dev-mods` strings. The skill body was not extracted; this is a gap.

## 3. Capability gap

| Need | Today | With a mod | Gain |
|---|---|---|---|
| Agent input state (empty / typed / suggestion) | `capture-pane` + regex (#8407 open) | `prompt.edit`, `prompt.suggest`, `prompt.fill` events give the state directly | Fixes #8407 by construction (inferred) |
| Session busy/idle, turn boundaries | Notification events + pane scraping | `turn.start` / `turn.complete` (`isAborted`, `durationMs`, `usage`) | Exact, push-based |
| Token burn per request and per subagent | statusLine payload; post-hoc analysis (#4837) | `turn.step` result `usage` with `e.agentId`; `$.session.usage()` | Live per-agent cost; `agent_cost` could act mid-run |
| Subagent start | `SubagentStart`/`SubagentStop` settings hooks | `agent.spawn` (can return `{ model }` or `{ deny }`) | Pre-start veto (advisory; see §6) |
| Build-slot visibility | `tm build-lease` CLI only | `AbovePrompt` band refreshed by `$.clock.every` | Operator sees queue state in session |
| Architect → session notices | Pane typing (now denied, #8902) / inbox | `session.receive` + `$.ui.toast`; `$.session.send` | Uses the sanctioned SendMessage channel |
| Operator commands mid-turn | `!tm …` shell escape | `$.command.register({ …, immediate: true })`, no Claude turn | `/tm-status` with no token cost |
| pm-guard escalation | Hard deny only | `$.ui.ask` holds the call; the wait does not count against the limit | "Ask the operator" for budget denials |
| pm-guard timeout | Timed-out `PreToolUse` "doesn't block the tool call" (hooks docs); registered `timeout: 10` | `tool.call` + `.catch` → `{ deny }`, and `$.process.run` time is outside the 10 s budget | A fail-closed layer around the same classifier (inferred design) |

## 4. Proposed integration (phases)

### 4.1 SPEC-MODS-01 — Phase 0: spike and version gate {#SPEC-MODS-01~draft}

**ID:** `SPEC-MODS-01~draft` · **Floor:** v2.1.287

- Add a `MODS_MIN_VERSION = (2, 1, 287)` gate next to `NATIVE_OUTPUT_STYLE_MIN_VERSION`, reusing `version_supports_native`'s fail-safe parse. Unknown version → no mod.
- Commit a throwaway mod under `content/` that only logs `turn.complete` to the debug log. Run `claude plugin validate --strict --json` and `claude plugin test` in CI on the pinned Claude Code version.
- Record the generated `.claude-plugin/types/claude-code/index.d.ts` header version as the tested version.

### 4.2 SPEC-MODS-02 — Phase 1: observe-only telemetry mod {#SPEC-MODS-02~draft}

**ID:** `SPEC-MODS-02~draft` · **Floor:** v2.1.287 · **Surfaces:** all, including `claude -p`

- Hooks: `turn.start`, `turn.step` (async generator, `yield* next(e)`), `turn.complete`, `agent.spawn`, `prompt.edit`, `prompt.suggest`, `session.measure`, `session.receive`. Every hook returns `next(e)` unchanged.
- Transport: one `$.http.fetch` POST per batch to the daemon's existing HTTP hook ingest, the same endpoint `tm hook` posts to. Alternative: `$.mcp.call` on `hook_event` (needs the MCP server listed in the plugin manifest; inferred). The daemon forwards `HarnessEvent` frames to the console bus (DOC-73).
- A heartbeat every 30 s via `$.clock.every`. The daemon marks a session `mod_telemetry: degraded` when heartbeats stop and falls back to capture-pane (§6).
- Retires: the input-state classifier for sessions with a live heartbeat. #8407 is closed by the mod path; the scraper stays as fallback.

### 4.3 SPEC-MODS-03 — Phase 2: operator interface {#SPEC-MODS-03~draft}

**ID:** `SPEC-MODS-03~draft` · **Floor:** v2.1.287 · **Surfaces:** terminal (all elements used are marked Terminal+Desktop)

- `AbovePrompt` band: build-lease slots held/queued and the session's clearance state. It is read-only and holds no lease logic.
- `$.ui.status`: one line for merge-queue / CI state. It replaces nothing, because the `tm statusline` segment stays.
- `$.ui.toast` for Architect messages arriving via `session.receive`, with `e.origin.kind` of `peer` or `peer-send-message`. The message still reaches Claude, so the hook returns `next(e)`.
- `/tm` commands registered with `immediate: true`, which shell out to `tm` via `$.process.run`.
- The Architect's own session gets a `Pane` (fleet table) fed by the daemon. This replaces the Architect reading capture-pane dumps for routine status.

### 4.4 SPEC-MODS-04 — Phase 3: guard hardening, additive only {#SPEC-MODS-04~draft}

**ID:** `SPEC-MODS-04~draft` · **Floor:** v2.1.287, and only after Phase 1 has run clean for one release

- The `tool.call` hook (matcher `Bash`, `Edit`, `Write`, `MultiEdit`) runs the existing classifier via `$.process.run(['tm','hook','--pm-guard-eval', …])`. `--pm-guard-eval` is a new argv form, because `$.process.run` takes no stdin per the docs (inferred). On deny it returns `{ deny }`. Its `.catch` returns `{ deny: 'pm-guard mod failed: ' + next.error.kind }`.
- The settings-hook `tm hook --pm-guard` stays registered and unchanged. It still runs after the mod chain, so a call passes only if both allow.
- Budget denials may use `$.ui.ask` to offer "allow once" instead of a hard deny. That needs an owner ruling (Q3).

## 5. What stays as-is

- **pm-guard as a settings `PreToolUse` hook.** It is the enforcement floor and works on every surface and Claude Code version. A mod is never the only gate (§6).
- **`SessionStart` / `SessionEnd` / `SubagentStop` settings hooks.** Resume-ID capture and slot release must survive a crashed or disabled mod.
- **The MCP server.** It is Claude's tool surface, works under `--safe-mode`, and is harness-neutral for #99.
- **The tmux driver.** It owns launch, `send-keys`, kill, and the pause/resume snapshot. Mods cannot launch or stop a session, and `session.end` hooks get 1.5 s in total, which is too short for a snapshot.
- **`tm statusline`, `outputStyle`, `--append-system-prompt-file`.** `prompt.section` could inject PM instructions, but text that changes between requests invalidates the prompt cache, and the appended file already works.
- **capture-pane scraping** as the fallback for: VS Code chat, Desktop WSL, `--safe-mode`/`--bare`, Claude Code < 2.1.287, a remote mods kill-switch, and Codex/Cursor sessions (#99).

## 6. Security and Fail-Open analysis

### 6.1 SPEC-MODS-05 — A mod is never a bypass {#SPEC-MODS-05~draft}

**ID:** `SPEC-MODS-05~draft`

The docs list three ways a user-tier mod defeats a non-managed `PreToolUse` hook such as pm-guard:

1. `tool.check` returning `allow` "can approve the call, unless the hook is in managed settings".
2. A `tool.call` hook that answers without calling `next` keeps "`PreToolUse` hooks from every other settings file" from running.
3. `$.process.run` and `$.fs.write` are not tool calls, so neither pm-guard nor deny rules see them. Deny rules hold over mods only where `sec-default@builtin` loads: with managed settings, or on Team/Enterprise plans.

This risk exists today, before trusty-mpm ships any mod. Any session can load a mod Claude writes into `dev-mods/` once the operator approves hot reload, or one passed via `--plugin-dir` / `CLAUDE_CODE_PLUGIN_DIRS`. Required controls:

- pm-guard denies PM and agent writes to `**/dev-mods/**`, to any `hooks/hooks.json` containing `modules`, and to `.claude-plugin/` outside `content/`. This joins the floor (#8878), so it holds under both bypasses.
- tm launch adds `CLAUDE_CODE_PLUGIN_DIRS` and `CLAUDE_CODE_PLUGIN_DIR_WATCH` to the spawn scrub (`core/claude_env_scrub.rs`) and passes no `--plugin-dir` except its own.
- `session_plugin_scope` (#7422) keeps default-deny for marketplace plugins. Whether it covers `dev-mods` is unverified (Q2).
- `tm doctor` reads the debug-log `hooks module … loaded` lines (or `/plugin`'s `N mods active` line). It reports any non-tm mod whose `validate` output lists `tool.check`, `tool.call`, or `$.process.*`. This is reported degradation, not a silent pass.
- The strongest control is managed settings with `pluginConfigs."cc-plugin-sec-default@builtin".options.allowManagedModsOnly: true` and `disableSideloadFlags`. These need an admin-owned managed-settings file (Q1).

### 6.2 SPEC-MODS-06 — Fail-Open Check {#SPEC-MODS-06~draft}

**ID:** `SPEC-MODS-06~draft`

| Failure | Documented behaviour | What advances anyway | Alarm, and how this design keeps it honest |
|---|---|---|---|
| Hook throws or times out before `next` | "Claude Code skips it, and the next handler runs" | The tool call | Phase 3 `.catch` → `{ deny }`; the settings pm-guard still runs |
| `.catch` handler exceeds 1 s | Not documented beyond the limit (assume skip) | The tool call | Settings pm-guard floor; never mod-only |
| Hooks worker crashes 3 times | "unloaded every mod that isn't built in" until `/reload-plugins` | Everything, unguarded by the mod | Heartbeat gap → daemon flags `mod_telemetry: degraded` and the console shows it; capture-pane fallback resumes |
| Remote kill-switch / `--safe-mode` / `disableAllHooks` | Mod not loaded | Session runs without mod | No heartbeat from launch → session marked "mods off", not "healthy". Note: user-settings `disableAllHooks` also stops pm-guard, an existing gap |
| Telemetry POST fails | Mod's own code | Mod counters | Mod buffers in `$.store` (4 MiB cap), counts drops, reports the count on the next heartbeat |
| Settings pm-guard times out (10 s) | "doesn't block the tool call" | The tool call | Existing gap; Phase 3 is the mitigation; regression test must time out the classifier and observe a deny |

Each row needs an error-arm test. `claude plugin test` can fire `tool.call` against a guard stubbed to throw or hang.

## 7. Distribution and versioning

- **Source of truth:** `content/mods/trusty-mpm/` (plugin dir: `.claude-plugin/plugin.json`, `hooks/hooks.json`, `hooks/register.ts`, `*.test.ts`), versioned with content releases (ADR-0064, `content-vX.Y.Z`).
- **Install:** `tm content install` copies the mod into a directory marketplace under the tm data dir. The launch settings writer then adds `extraKnownMarketplaces` (`source: directory`) and `enabledPlugins` to the tm-owned config dir. Plugin id: `trusty-mpm@trusty-mpm-local` (inferred name). It runs at user tier. "Organization" tier needs managed settings plus an admin-only directory.
- **Development:** `tm launch --dev-mod <dir>` passes `--plugin-dir`, which gives hot reload.
- **Not chosen:** a public GitHub marketplace. Plugins copied into the cache "count as a user's", and the update cadence is decoupled from `tm`.
- **Version pin:** `plugin.json` `version` equals the content version. The manifest records `tested_claude_code = "2.1.287"`. tm enables the mod only when `claude --version` ≥ `MODS_MIN_VERSION`. Above the tested version it still enables, but `tm doctor` warns until CI re-validates against the newer generated types. Fleet skew: each session's daemon record carries the Claude Code version and mod version from the heartbeat.

## 8. Open questions for the owner

1. **Managed settings.** May `tm install` write an admin-owned managed-settings file (`allowManagedModsOnly`, `disableSideloadFlags`, `prependPlugins` for the tm guard mod)? Without it, any user mod can override pm-guard (§6.1).
2. **Dev-mods.** Should pm-guard block Claude-written mods outright in managed sessions, or only flag them? (`enabledPlugins` coverage of `dev-mods` is unverified.)
3. **Ask, don't deny.** May Phase 3 turn budget denials into `$.ui.ask` prompts? Headless runs reject `ask`, which falls back to deny.
4. **Sequencing.** Ship as its own post-2.0.0 milestone, or fold Phase 1 into #99 as the Claude Code half of the harness-event abstraction?
5. **Desktop.** Is the Desktop Code tab a supported trusty-mpm surface? It decides whether Phase 2 tests the `desktop` surface.

## 9. Sources (accessed 2026-10-02)

- Mods overview — https://code.claude.com/docs/en/plugins/mods/overview
- Create a mod — https://code.claude.com/docs/en/plugins/mods/create
- React to events — https://code.claude.com/docs/en/plugins/mods/events
- Draw in the interface — https://code.claude.com/docs/en/plugins/mods/interface
- Use the mods API — https://code.claude.com/docs/en/plugins/mods/api
- Manage mods for your organization — https://code.claude.com/docs/en/plugins/mods/admin
- Troubleshoot a mod — https://code.claude.com/docs/en/plugins/mods/troubleshoot
- Mods reference — https://code.claude.com/docs/en/plugins/mods/reference
- Permissions, "Extend permissions with hooks" — https://code.claude.com/docs/en/permissions
- Hooks (timeout behaviour) — https://code.claude.com/docs/en/hooks
- Changelog, v2.1.287 — https://code.claude.com/docs/en/changelog
- Local: `claude --version` → `2.1.287 (Claude Code)`; binary `~/.local/share/claude/versions/2.1.287`
- Repo: [`docs/roadmap/trusty-mpm.md`](../roadmap/trusty-mpm.md); milestones #97, #99 (GitHub API, 2026-10-02)
