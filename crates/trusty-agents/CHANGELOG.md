# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Security

- **Loopback-only doctrine (#3329):** the HTTP API server (`--api`/`--serve`)
  now binds `127.0.0.1` by default instead of `0.0.0.0`. A non-loopback bind is
  an explicit opt-in via the new `--bind <addr>` flag, and the server **refuses
  to start** on a non-loopback interface unless an API token is set
  (`--api-token` / `TAGENT_API_TOKEN`) — the error points operators at the
  trusty-console proxy (`/api/agents/*`) as the intended remote path. The API
  can spawn arbitrary subprocesses, so an unauthenticated LAN-reachable bind was
  a real exposure. Additionally adopts the shared same-origin write guard
  (`trusty_common::server::with_guarded_middleware`, mirroring #3317) router-wide
  so cross-origin browser writes (CSRF) to `POST /api/task`, the `/api/tm/*`,
  `/api/ctrl/*`, and `POST /rpc` surfaces are rejected with 403; GET reads, the
  `/api/events` SSE stream, and same-origin/loopback writes are unaffected. The
  agents-ui webview keeps working: in Tauri desktop mode writes travel over
  Tauri IPC (never HTTP), and in browser mode the SPA is served same-origin
  loopback. Replaces the crate-local CORS/compression/trace stack with the
  shared standard middleware so trusty-agents no longer drifts from the sibling
  trusty-* daemons.

### Added

- The API server now writes the standard `http_addr` discovery file on bind
  (and removes it on graceful shutdown), so the trusty-console reverse proxy can
  resolve the agents surface at `/api/agents/*` (#3331).

### Fixed

- Committed `ui/pnpm-lock.yaml` (architecture-review tranche 0): the file was gitignored with a comment claiming the repo-wide detect-secrets pre-commit hook flags npm integrity hashes, but the root `.pre-commit-config.yaml` already excludes `.*pnpm-lock\.yaml` (only the crate-scoped `crates/trusty-agents/.pre-commit-config.yaml`, which governs Python tooling and is unrelated to the JS UI, was missing that exclusion) and no CI workflow runs detect-secrets at all. Every other Tauri UI in the workspace (`trusty-mpm-gui`, `trusty-search`, `trusty-memory`, `trusty-analyze`, `trusty-code-gui`, `trusty-console`) already commits its own subdir-scoped `pnpm-lock.yaml`; `trusty-agents/ui` was the only outlier. Removed the stale `.gitignore` entry and regenerated the lockfile with `pnpm install --lockfile-only`.

### Added

- `PATCH /api/agents/:name` — persists a per-agent `model_id`/`provider_id` override to that agent's `.trusty-agents/agents/<name>.toml`, editing the `[agent]` table in place via `toml_edit` so every other key, comment, and prose block (e.g. a long system-prompt) survives untouched. `provider_id` is validated against the `trusty_common::inference::registry` catalog surfaced by `GET /api/models` (#3243); a `runner = "claude-code"` agent rejects any model/provider that doesn't resolve to Anthropic, since the local `claude` CLI only talks to Anthropic. Returns the updated agent in the same shape `GET /api/agents` uses, so a client can round-trip through either route. Pairs with the agent create/edit UI merged in #3279 (closes [#3246](https://github.com/bobmatnyc/trusty-tools/issues/3246), part of epic #3052)
- new bundled base `assistant` agent (`.trusty-agents/agents/assistant/`) — a reusable, **nameless** personal-productivity template derived from the "Izzie" prototype: functional `role = "assistant"`, the curated Google Workspace tool surface, `memory.read`/`memory.write`/`search.read` scopes plus **read-only** `google.read` (§5.5 — a generic template does not mutate a user's Google data by default), and generic helpful/productivity instructions, with NO persona name, user-identity binding, or user-specific skills. Persona identity is contributed by a user's `extends = "assistant"` personalization overlay (Eve §2.5.1). Izzie is now shipped as the reference overlay example (`.trusty-agents/agents/izzie/`): `extends = "assistant"` + personal deltas (display name, personal skills, Masa-bound persona) and an explicit opt-in to Google **write** (`google.*`). The `extends` inheritance resolver lands in [#3055](https://github.com/bobmatnyc/trusty-tools/issues/3055); until then the overlay carries the curated `[tools]` allowlist/scopes and the safety-critical persona guardrails (approval-framing, anti-hallucination) redundantly so it is safe standalone, and the working Izzie the REPL/Telegram persona paths load remains the standalone `izzie.toml` (closes [#3054](https://github.com/bobmatnyc/trusty-tools/issues/3054), refs #3052, #3055)

- `extends:`-based agent personalization (DOC-41 §2.5 / §2.5.1, epic #3052): a user can now personalize a stock/bundled base agent — name it, add tools, override tone — without forking it, by authoring a child agent (e.g. `~/.trusty-agents/agents/my-assistant.md`) that declares `extends: <base>` and layers personal deltas on top. The previously-inert `extends:` frontmatter key is now live. Resolution mirrors trusty-mpm's proven `compose_agent` exactly (same `MAX_DEPTH = 8`, case-insensitive base lookup, base-first prose concatenation) and runs once at load time — reachable from BOTH the informational `AgentRegistry::load` AND the real dispatch loaders `AgentConfig::by_name`/`by_name_async` (a flat `<name>.md` overlay tier was added to both so a personalization overlay actually dispatches, not just shows in the roster) — never per dispatch. Merge follows the §2.5 table: scalars (`model`, `display_name`, `role`, `description`, plus `runner` and opt-in `persistent_session`) child-overrides-parent; list fields (`tools.allowed`, `tools.scopes`, `system_prompt.skills`, `capabilities`) union (dedup, base-first); persona/instruction prose concatenates base-first; `llm`/`compress`/`session`/`plugins`/`rbac` inherit from the base wholesale. Cycles, missing parents, and over-deep chains fail with a clear `AgentExtendsError`; in the registry a failed resolution keeps the unresolved agent but surfaces the breakage in the roster / `tagent agents list` (new `AgentSummary::extends_error`). A directory package whose `extends` can't be resolved no longer silently shadows a complete flat `<name>.toml` — the loader falls back to the flat file with a warn. Case-only duplicate agent names are rejected at load (they broke case-insensitive resolution). `.md` agent frontmatter now also parses `tools:` and `display_name:` so a personalization overlay's declared tools actually union with the base's (closes [#3055](https://github.com/bobmatnyc/trusty-tools/issues/3055))

### Changed

- The REPL startup splash (`repl::tui::banner::banner_lines`, and its dormant widget-mode counterpart `draw_banner`) now renders the shared trusty splash art (`trusty_common::banner::TRUSTY_SPLASH_ART`, per-glyph shaded via `shade_bucket`) instead of a bespoke robot-glyph design, so `tagent`'s REPL presents the same trusty branding as `tm`'s launch banner (closes [#3326](https://github.com/bobmatnyc/trusty-tools/issues/3326)). The left column is now sized from the shared art's actual width instead of a fixed 18-col floor. tagent's own contextual text (identity line, recent activity, commands) is unchanged.
- `gworkspace` `[[tool_registry.endpoints]]` in the bundled default config now
  ships `enabled = true` (was `false`, "pending auth"). `rpc.discover` on the
  `trusty-gworkspace-mcp` server is a pure static function that never touches
  the token store, so eager discovery at harness startup always succeeds
  regardless of auth state; unauthenticated tool calls fail with a clear
  "run setup" operational error instead of a startup crash. Authenticate with
  `trusty-gworkspace-mcp setup` to actually light up Gmail/Calendar/Drive
  access for the Assistant (Phase 1 of the Assistant epic #3052, part of
  [#3056](https://github.com/bobmatnyc/trusty-tools/issues/3056))

### Fixed

- REPL `/agent <name>` (`handle_agent_command_into`, `crates/trusty-agents/src/repl/agent_commands.rs`) now resolves personas via the same directory-package + `extends`-aware loader the one-shot `--direct` path already used, instead of a hardcoded flat-file check (`agents_dir.join("{name}.toml")`) — so `/agent assistant` now activates the base Assistant persona, which ships only as a directory package (`assistant/agent.toml`, no flat `assistant.toml`). `/agent` with no argument (`list_assistant_agents_into`) now surfaces directory-package agents too, not just flat `*.toml` files, and `/switch assistant` is a recognized alias alongside ctrl/izzie/cto. Resolution is anchored to the REPL's own resolved `agents_dir` via a new `AgentConfig::by_name_in(dirs, name)` (the default-dirs `AgentConfig::by_name` is now a thin wrapper over it) rather than `by_name`'s process-global `TAGENT_CONFIG_DIR`/CWD dirs, so a standalone launch (CWD outside the project) still activates exactly what it just listed. `by_name`/`by_name_in` (and their async twin) also now reject an agent name containing `/`, `\`, or an exact `.`/`..` segment before any `dir.join(name)` join — closing a path-traversal gap where `/agent ../../foo` could read `agent.toml`/`persona.md` outside the intended agents directory (closes [#3303](https://github.com/bobmatnyc/trusty-tools/issues/3303), refs #3052)
- `run_pm_task_with_persona` (the dispatch path a selected named persona/Assistant chat turn takes) now reaches tool parity with the session path (`run_pm_task_with_history`) — delegation (`delegate_to_agent`), CTRL project-management (`add_project`, `list_projects`, `remove_project`, `stop_task`, `set_active_project`), filesystem (`move_file`, `create_dir`), search (`search_code`), shell (`run_bash`), and `tm` tools are now registered alongside the MCP/git/ticketing/live-MCP tools it already wired. Access is still gated per persona: a tool is only advertised to the LLM when its name matches the persona's `[tools].allow` glob list (extracted into the new pure `filter_persona_tool_names` helper, pinned by 4 unit tests) and passes the existing RBAC-tier filter — registering a tool is not the same as granting it, so no persona gains capability beyond what it already declares. Full `[tools].scopes` enforcement remains tracked separately by [#3208](https://github.com/bobmatnyc/trusty-tools/issues/3208) (closes [#3285](https://github.com/bobmatnyc/trusty-tools/issues/3285))
- `llm::credentials::pick_credentials()` now resolves `openrouter`/`anthropic`/`claude-code` via the shared `trusty_common::inference::credentials::resolve_key` 3-tier resolver (env > `.env.local` > secure keyring/file store) instead of a raw `std::env::var` read — a credential configured only via `tagent config keys set` was previously invisible at chat dispatch even though `trusty-channels` and this crate's own `GET /api/models` catalog already consulted the shared store (closes [#3248](https://github.com/bobmatnyc/trusty-tools/issues/3248))
- `tmux::orchestrator::TmuxOrchestrator::create_session` and `debugger::tmux::TmuxAdapter::create_session` now apply generous tmux scrollback (`history-limit=100000`) and mouse-wheel scrolling BEFORE `new-session`, via the new shared `trusty_common::tmux` layer — this crate previously ran its own independent tmux implementation with no scrollback handling at all, so every trusty-agents-hosted tmux session (including the debug REPL) was stuck at tmux's tiny 2000-line default even after trusty-mpm's #2398/#2399 fix landed (closes [#3004](https://github.com/bobmatnyc/trusty-tools/issues/3004), refs #2398, #2399)
- migrate off archived/unsound `serde_yml` (GHSA-hhw4-xg65-fp2x) and its transitive `libyml` (GHSA-gfxp-f68g-8x78) onto the workspace-blessed `serde_yaml` 0.9, closing Dependabot alerts #42/#43 (closes [#2991](https://github.com/bobmatnyc/trusty-tools/issues/2991))
- bump `lru` 0.12 → 0.16, fixing RUSTSEC-2026-0002 (`IterMut` violates Stacked Borrows, GHSA-rhfx-m35p-ff5j); no call-site changes needed ([#2782](https://github.com/bobmatnyc/trusty-tools/pull/2782)) ([`e62b454`](https://github.com/bobmatnyc/trusty-tools/commit/e62b4540d39c5a442d05e849197157932f37e664))
