# Changelog — trusty-code

All notable changes to trusty-code are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Fixed

- the unified-diff applier (`tools/fs/edit_format/diff.rs`) no longer errors
  on a `git diff`-style `\ No newline at end of file` footer marker inside a
  hunk body — the marker is metadata, not content, so it no longer fails the
  whole apply. Its *position* (after a `-`, `+`, or ` ` line) is also tracked
  so the applier picks the OUTPUT's trailing-newline state correctly instead
  of always copying the original file's, which silently corrupted the
  trailing byte on either direction of a no-trailing-newline state change
  (closes #2150).

### Added

- **Embedded default agents converted from TOML to `.md`+frontmatter (#2897,
  epic #2892, Slice C).** The three bundled default agents
  (`engineer`/`qa-agent`/`code-reviewer`, #2895) are now authored as
  `crates/trusty-code/src/assets/agents/*.md` instead of `*.toml`; the retired
  TOML files are deleted. The embedded fallback in `agents::load_all_agents`
  now projects each `.md` string via a new
  `agents::md_loader::project_embedded_md`, which shares the exact same
  frontmatter -> `AgentConfig` mapping (`project_to_agent_config`) that the
  disk `.md` loader (`load_md_agent`) uses — the only difference is that the
  embedded path skips `compose_agent`'s `extends:`-chain resolution (there is
  no source directory for a compiled-in `&'static str` to resolve against,
  and none of the three defaults declare `extends:`), calling
  `agent_metadata_from_str`/`extract_body` directly on the raw string
  instead. Every default's projected `AgentConfig` is field-identical to what
  the retired TOML produced — same `name`, `model` (`None`), `max_tokens`,
  `tools.allowed` (exact allowlist), and `system_prompt.content` (the prose
  body is byte-identical modulo the shared `.md` body-extraction path's
  pre-existing trailing-whitespace trim). Purely additive/non-breaking: the
  TOML loader for user `.claude/agents/*.toml` configs (`AgentConfig::load`,
  `AgentConfig::from_toml_str`) is untouched; TOML retirement there is a
  later slice (D).
- **Markdown+frontmatter `.md` agent loader, dark-launched alongside TOML
  (#2897, epic #2892, Slice B).** `agents::md_loader::load_md_agent` composes
  a `.md` agent source file's `extends:` chain via
  `trusty-agents-common::agents::builder::compose_agent` (Slice A, #2952) and
  projects the result onto tcode's existing `AgentConfig`: `model` ->
  `agent.model`, `max_tokens` -> `llm.max_tokens`, `tools:
  Option<Vec<String>>` -> `ToolsConfig.allowed` (a direct map — both sides
  share identical `None`=all-allowed / `Some([])`=deny-all /
  `Some(list)`=allowlist semantics), and the composed prose body ->
  `system_prompt.content`. The composed frontmatter's HR-1 role-derived
  `initialPrompt` enrichment (keyed off trusty-mpm's role table, which
  collides by name with tcode's `engineer`/`qa` roles) is guaranteed to never
  leak into `system_prompt.content` — the body extraction only reads past the
  closing frontmatter fence and the public `AgentMetadata` projection has no
  `initial_prompt` field at all. `discover_agents`/`load_all_agents` now scan
  and load both `*.toml` and `*.md` agent files; when both exist for the same
  agent name, the `.toml` deterministically wins (a warning is logged). Purely
  additive and behavior-preserving — the TOML loader is unchanged; TOML
  retirement is a later slice (D).
- **Engineer receives `use_skill` on the `--legacy-in-process`/`run_task` path (#2942).** The `python-engineer` role's tool registry (`ProjectToolFactory` in `run_task/mod.rs`) now conditionally registers `UseSkillTool` alongside its other project tools when a `SkillResolver` is available, so the engineer can fetch a skill's full body mid-turn instead of relying solely on the catalog summary baked into its system prompt.
- **Native skill discovery on the `--legacy-in-process`/bake-off `run-task` path (#2924).** The `use_skill` tool and the `.claude/skills/` catalog now reach the PM's prompt and tool registry on `run_task::execute_run_task`, not just the daemon/thin-client path — a PM run through `run-task` can now discover and load project skills the same way a daemon session already could.
- **Embedded default agents & skills, disk-first with embedded fallback
  (#2895).** A project with no `.claude/agents/` and/or `.claude/skills/`
  (or an empty one) previously started with zero agents and zero skills.
  `crates/trusty-code/src/assets/` now bundles three native-TOML default
  agents (`engineer`, `qa-agent`, `code-reviewer`) plus trusty-mpm's 28
  universal skills (format-identical `SKILL.md` files, reused verbatim;
  `tm-*` orchestration skills excluded since they drive trusty-mpm MCP tools
  tcode does not have) at compile time via `include_str!`.
  `agents::load_all_agents` falls back to the embedded set when the
  *parsed* result is empty (a directory with only unparseable TOML still
  falls back, not just a missing/empty one); `skills::discover_skill_metadata`
  falls back when the disk scan is empty. Either way, any successfully
  discovered disk config — even a single file — is used as-is and never
  merged with the embedded defaults.
- **Stable per-spawn `agent_id` for event attribution (DOC-39 AC-13.1/13.2).**
  Agent attribution on the event stream was keyed only by `agent: String` (the
  agent-config name), so two concurrently-delegated same-named agents (e.g.
  two `python-engineer` delegations) were indistinguishable. Every
  agent-attributed event (`ToolStarted`/`ToolFinished`/`ToolError`,
  `SearchPerformed`, `MemoryRecalled`, `AgentSpawned`/`AgentStarted`/
  `AgentDone`/`AgentFailed` — the latter four have the field defined now but
  are not yet emitted by any production call site) now also carries
  `agent_id`: a UUID v4 minted once per delegation spawn
  (`runner::in_process::InProcessAgentRunner::run_pipeline`)
  and a stable, session-scoped id for the PM/root agent
  (`task::executor::run_and_record`). Additive and non-breaking — `agent`
  is unchanged and unremoved; `#[serde(default)]` on `agent_id` keeps old
  recorded transcripts deserializing (as an empty string, not a sentinel).

### Changed

- **BREAKING — the TOML agent loader is retired; `.claude/agents/*.toml` is
  no longer read at all (#2897, epic #2892, Slice D).** `AgentConfig::load`
  and `AgentConfig::from_toml_str` are deleted, along with the `toml` crate
  dependency. `discover_agents` now globs `*.md` ONLY — a project whose
  `.claude/agents/` still holds `.toml` files gets a LOUD, aggregated
  `tracing::warn!` naming every orphaned file and pointing at the new
  converter script, then those files are skipped (never parsed, never
  erroring). If disk holds nothing but orphaned `.toml` (or nothing at all),
  `load_all_agents` falls back to the embedded `engineer`/`qa-agent`/
  `code-reviewer` defaults exactly as it does for an empty directory — a
  project is never left with zero agents. **Migration:** run
  `scripts/migrate-tcode-agents-toml-to-md.py <path-to-.toml-or-dir>` to
  convert existing `.toml` agents to the `.md`+frontmatter format (dark-launched
  in Slice B, #2897) that has been the primary format since Slice C; delete
  the `.toml` sources once you've reviewed the generated `.md`. This
  completes the #2897 epic (#2892): TOML -> `.md` is now a one-way door.
- **trusty-code now depends on `trusty-agents-common`; `ServiceTier`,
  `RunContext`, and `HistoryMessage` are re-exported instead of redeclared
  (#2893, epic #2892).** `crates/trusty-code/src/tools/traits.rs` re-declared
  ~150 lines of trait/type definitions that already exist in
  `trusty-agents-common` (the same crate `trusty-agents` already re-exports
  these from). `ServiceTier` and `RunContext` were byte-identical;
  `HistoryMessage` was a strict field subset. All three are now
  `pub use trusty_agents_common::...` re-exports — no behavior change.
  `ToolExecutor`/`ToolResult` and `AgentRunner`/`AgentOutput` intentionally
  stay LOCAL: tcode's `ToolResult` carries a `telemetry` field (#2862) and
  `AgentOutput` carries a `finish_status` field (#2683, whose `usage:
  TokenUsage` also carries a `cost_usd` field, #50) that the shared
  definitions lack, and unifying them would require either a much larger
  events-module move or changes to every `trusty-agents`/`cto-assistant`
  call site — out of scope for this behavior-preserving refactor.
- **BREAKING — projectless mode + one typed project binding (UI Phase-1).**
  "Project" was one concept split across two disagreeing API surfaces:
  `task.run` took a REQUIRED `project: PathBuf` (`task/protocol.rs:48`), while
  `session.create` took an untyped, free-form `project: Option<String>` LABEL
  (`session/protocol.rs:157`) that was never validated, never bound, and
  disconnected from the path `task.run` demanded — a label that could not be
  indexed and a path that could not be omitted, i.e. two halves of one missing
  object. Because the path was required, a **projectless** workstream was
  inexpressible, and the shell's entry screen (spec DOC-39 §4.2/§5.5 screen 7a,
  "Open a project" — a workstream that exists BEFORE a project is chosen) was
  literally unimplementable. Both surfaces now converge on one typed
  `binding::ProjectBinding` with the spec's **three** states:

  | State | Indexing | Git affordances |
  |---|---|---|
  | `projectless` — no directory bound (chat/planning) | none | none |
  | `directory` — bound, non-git (#2728/#2747) | **yes** | none |
  | `git_repo` — bound git worktree | yes | full |

  **Binding is NOT gated on `.git`.** The design proposal's own text ("binds the
  moment work touches files in a git repo") is wrong and contradicts shipped
  behaviour: it would exclude the non-git working dirs (#2728) and OS temp dirs
  (#2747) we deliberately support. A non-git directory BINDS and INDEXES; the
  git/non-git split decides only which git affordances are offered, never
  whether the project binds. Git detection is now a single implementation
  (`binding::is_git_worktree`), which `run_task::diff` delegates to, so a
  `git_repo` binding can never disagree with the diff strategy.

  What projectless does with the three things that assumed a project — each a
  defined behaviour, never a panic: indexing is **skipped**; the project-scoped
  memory palace is **skipped** (`memory_sink_for` takes `Option<&Path>` and
  returns `None` — the scratch root is deliberately NOT substituted, which would
  mint an orphaned palace per run); and the fs/bash tools are rooted at an
  **ephemeral scratch dir**, discarded when the run ends and logged at `warn` to
  stderr so a projectless write is observable rather than silent. `CLAUDE.md`,
  catch-up, skills, and the `settings.json` mode tier already degraded to
  "absent" and needed no projectless special-casing.

  Breaking-change surface, and why each is safe or deliberate:
  - **`task.run`'s JSON-RPC params are UNCHANGED** — its `project` was a
    `register()` argument (daemon-scoped), never a request field, so no
    JSON-RPC caller breaks. Its response gains a `binding` object.
  - **Rust API (breaking):** `serve::{build_router, run_stdio, run_http}` and
    `task::protocol::register` take `ProjectBinding` instead of `PathBuf`;
    `TaskRunParams.project` → `.binding`; `SessionRegistry::create`'s third
    param is a `ProjectBinding`; `mode::resolve_mode` takes `Option<&Path>`.
  - **`session.create` wire (breaking, deliberate):** `project` is now a project
    PATH, not a decorative label — a string that names no directory returns
    `-32003 invalid_argument`. Erroring is the point: silently accepting a
    "project" that binds and indexes nothing is the exact failure this
    reconciliation ends. Omitting `project` remains valid and means projectless.
  - `Session` gains a typed `binding`; its `project` field survives as a
    display label but is now DERIVED from the binding (`ProjectBinding::label`),
    so the two can never again disagree. `binding` is `#[serde(default)]`, so an
    older payload without it reads back as projectless.

  Being breaking at the Rust-API and `session.create`-semantics level, the next
  release must be **0.3.0** (pre-1.0: minor = breaking), not a patch bump.
  ([#2855](https://github.com/bobmatnyc/trusty-tools/pull/2855), spec DOC-39
  §4.2/§5.5, ACs 2.1–2.4 / 16.1–16.3)

### Added

- **`tcode serve --project` is now OPTIONAL** — omit it to serve projectless.
  Given, it must be an existing directory (validated at the boundary with an
  actionable hint, rather than failing later as a confusing per-task error); it
  need NOT be a git repo. A projectless daemon resolves agents from the
  user-level `~/.claude/agents` rather than the process CWD, which would
  silently bind a directory the operator never chose.

- **Daemon-side directory inspection API (`fs.list_dir`) for the UI's project
  picker (UI Phase-1, screen 7a)** — a new `fs.*` JSON-RPC namespace on `/rpc`,
  alongside `session.*`/`task.*`. `fs.list_dir(path?, include_hidden?)` returns
  `{path, display_path, parent, entries[{name, path, is_dir, is_git_repo}]}`.
  The UI is a thin client — no UI target touches the filesystem directly,
  **including Tauri** (which could, but must not: that would let the desktop
  build do something the web build cannot, forking one UI into two behaviours) —
  so browsing local disk to pick a project has to be a daemon call. One response
  serves both jobs: `display_path`/`parent` drive the breadcrumb and
  up-navigation, while `is_git_repo` is simultaneously 7a's `git` badge and the
  discriminator for the three-state project binding model (projectless → non-git
  dir → git repo). A non-git directory is a first-class success with
  `is_git_repo: false`, never an error (#2728). `~` is expanded and paths
  canonicalized server-side, matching the `expand_tilde` convention already used
  in `trusty-mpm` and `trusty-agents`; `path` defaults to `~` so the projectless
  cold start is just `fs.list_dir({})`.
  - Git-ness handles **both** on-disk shapes: `.git` as a directory, and `.git`
    as a FILE carrying a `gitdir:` pointer — which is what a linked worktree (and
    a submodule) has. An `is_dir()`-only check badges every worktree as non-git;
    that is the bug PR #2839 hit in `build.rs`, and this workspace's own
    checkouts are linked worktrees. Detection is two `stat`s (plus one small read
    for the rare `.git`-file) rather than a `git rev-parse` subprocess per entry,
    because this runs across every row of a directory on an interactive picker's
    hot path.
  - Errors are typed and distinguishable rather than stringly, reusing the vision
    spec's existing §13.2 taxonomy instead of inventing `fs`-specific codes:
    `-32002 not_found` (no such path), `-32003 invalid_argument` (path is a
    file), `-32001 permission_denied` (OS refusal), `-32603 internal` (other IO).
    New `RpcError::not_found`/`permission_denied` constructors back the first
    two.
  - **No path guard, deliberately** — no denylist, sandbox root, or permission
    layer, and no macOS TCC state machine. tcode is a local app: the daemon runs
    as the user with the user's own entitlements, so a listing discloses nothing
    `ls` would not. Decisively, the same API already exposes `task.run`, which
    executes arbitrary code as the user — guarding browse while that sits one
    method away is ceremony, not security. An OS-level refusal is reported as an
    ordinary typed error. Recorded as a module doc comment so it is not later
    "fixed".

- **`index_readiness` event — a warming index is no longer indistinguishable from
  "ready, zero hits" (UI Phase-1, follows [#2784](https://github.com/bobmatnyc/trusty-tools/issues/2784)).**
  The per-lane index readiness `trusty_common::search_readiness` already computed
  at task start was stderr-only, so no API consumer — including tcode's own SPA —
  could reach it. The daemon path now publishes it as `Event::IndexReadiness`
  (replayable through the session ring buffer, streamed over
  `GET /sessions/{id}/events`). Its `state` field (`"ready"` | `"warming"` |
  `"unavailable"`) is the fix for the concrete failure this addresses: during
  semantic warm-up a search returns EMPTY, which looks identical to a fully-ready
  index that genuinely has no match — opposite meanings that led a model to
  conclude "nothing there" and hand-explore to the wrong target. A UI must not
  render "no results" unless `state == "ready"`. Per-lane
  `lexical_ready`/`semantic_ready`/`graph_ready` flags, `lifecycle_status`, and
  `chunk_count` ship alongside. The CLI (`run-task`) path is unchanged and stays
  log-only; `probe_index_readiness`'s fail-open contract is preserved — a `None`
  probe reports `state: "unavailable"` (also not evidence of absence) rather than
  going silent.
- **`context_budget` event — the Infinite Sessions guarantee is now renderable
  (epic [#2343](https://github.com/bobmatnyc/trusty-tools/issues/2343)).**
  `agent_loop::cadence` enforces "working context >= 60%, session overhead <= 40%"
  every single turn and returned a `CadenceOutcome` documented as existing "for
  observability/tests" — which the sole call site then discarded, so nothing could
  observe the guarantee it enforces. The outcome now carries the REAL measured
  `overhead_tokens` (`enforce_budget` returns the `estimate_total_tokens` value it
  already had to compute, rather than dropping it) and is published as
  `Event::ContextBudget`: context window, measured overhead, cap, and the derived
  `working_context_pct`/`overhead_pct` a live budget meter renders, plus
  `compaction_fired`/`compaction_rounds` to make a compaction legible as *removed
  from context, not from the record*. Emitted only by the PM's persistent-session
  loop — cadence's PM-only gating (`AgentLoopConfig.cadence` defaults `None`) is
  unchanged, so a delegated engineer loop never emits.
- **Agent attribution on every tool event (UI Phase-1 API)** — `ToolStarted`,
  `ToolFinished`, and `ToolError` now carry an `agent` field naming the agent
  that dispatched the call. Previously a client could only guess whether a tool
  call came from the PM or a delegated engineer by interleaving the event stream
  against `AgentSpawned`/`AgentDone` ordering — fragile inference that breaks as
  soon as their calls overlap, and the reason the UI could not answer "which
  agent drove this change?". The name is a per-call PARAMETER on
  `agent_loop::ToolEventSink` rather than sink state, because ONE
  `Arc<dyn ToolEventSink>` is shared by the PM's loop and every delegated
  sub-agent's loop (`task::executor::run_and_record` clones the same handle into
  both), so a sink carrying its own name could only ever report one of them. The
  dispatching `AgentLoop` is the only layer that knows its own identity, so it
  passes it per call — declared via the new `AgentLoop::with_agent`, wired at
  both production sites (PM: `params.agent_name`, default `"pm"`; delegated
  sub-agents: the runner's `agent_name`). `agent` is the agent NAME, matching the
  key the rest of the taxonomy already uses (`AgentSpawned.agent`,
  `PmDelegating.agent`, `LlmRequested.agent_name`), so the UI can join them
  directly; loops that declare no agent emit the documented
  `events::UNATTRIBUTED_AGENT` (`"unknown"`) sentinel rather than an empty string
- **`search_performed` event — structured search telemetry** — a new `Event`
  variant carrying `{agent, lane, query, hit_count, latency_ms}`, emitted by
  `search_code` ALONGSIDE (never instead of) the generic tool events, so existing
  consumers see an unchanged stream. `lane` is the lane trusty-search ACTUALLY
  served, not the mode the model requested: a `semantic`/`symbol` query against a
  still-building index transparently retries on the lexical lane
  ([#2783](https://github.com/bobmatnyc/trusty-tools/issues/2783)) and now
  reports `lane: "lexical"` rather than claiming a semantic search that never
  ran. `hit_count` is `null` when the payload shape could not be counted — never
  a misleading `0`, which would read as "no hits". The fail-open paths (absent
  daemon, no index) attach no telemetry at all: they never reached a lane, so
  there is no search to report
- **`memory_recalled` event — structured recall telemetry with the `injected`
  flag** — a new `Event` variant carrying
  `{agent, query, results: [{score, injected}]}`, emitted by `recall_session`.
  `injected: false` means exactly "recalled but not entered into context": the
  tool drops WHOLE lowest-scored results to fit its token budget, and that
  decision — plus every result's score — previously died inside the tool's
  rendered text. This is what lets a UI show which memories reached the model and
  which were held back
- **`ToolResult::with_telemetry` / `ToolResult::telemetry`** — the seam that
  carries a tool's structured account (`tools::telemetry::ToolTelemetry`) to the
  agent loop, which forwards it to the new `ToolEventSink::tool_telemetry` hook.
  Tools stay pure functions of their arguments — no sink, no session, no agent
  name — so attribution keeps ONE source of truth and each tool stays unit-
  testable without a bus. `ToolResult::Success` is now a struct variant
  (`{text, telemetry}`); `ToolResult::ok`/`content`/`is_error` are unchanged, so
  no tool call site moved. `tool_telemetry` has a default no-op body, leaving
  every pre-existing sink impl compiling untouched
- **Build provenance in `--version` and `tcode_report.json`** — `tcode --version`
  now prints the git SHA and commit date alongside the semver
  (`tcode 0.2.0 (b20adfca 2026-07-16)`), and `tcode_report.json` carries a
  `build` object (`{version, commit, commit_date}`); the human `run-task`
  summary gains a matching `build:` header line. Previously a run's artifacts
  could not be attributed to the binary that produced them — a semver alone
  collapses every commit on a branch into one string — which during the
  2026-07-16 L4 validation forced provenance to be reverse-inferred from
  `cargo install`'s mtime reset and produced a WRONG "bug still recurs"
  conclusion, retracted only after a dedicated forensic check. Provenance is
  captured by `build.rs` from `git rev-parse --short HEAD` / `git log -1`, using
  the COMMIT date rather than a build wall-clock so rebuilds stay reproducible,
  and degrades to `"unknown"` (never a build failure, never `null`) outside a
  git checkout — the crates.io-tarball path
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))

### Fixed

- **P1: the re-delegation cap counted ALL delegations, not retries — killing
  runs before the build started.** `MAX_REDELEGATIONS = 3` was documented as
  bounding *retries after failure*, but the counter incremented on every
  engineer invocation run-wide and was never reset per call, so a *successful*
  delegation permanently consumed budget a later one then lacked. A PM using
  delegation for legitimate reconnaissance — the normal PM shape — was silently
  guillotined: the 4th call was refused **without the inner runner ever being
  invoked** (zero engineer turns, no code written), and the latched signal then
  fired `run_task`'s `with_stop_signal`, halting the PM loop at the next turn
  boundary and making it unrecoverable by design. L4 bake-off run-6 issued 3
  read-only recon delegations and had its 4th — the actual build — refused,
  ending `partial`/exit 6 with 0/9 tests and 0/5 deliverables; runs 3/4/5
  succeeded only by luck of issuing one fewer recon call. Measured cost: 2-in-7
  total-loss runs caused by the harness rather than the model. The cap now
  counts retries: `MAX_REDELEGATIONS` is checked against a counter **local to
  each delegation**, reset at loop entry, so a successful delegation never
  spends a later one's budget. Exhausting it is now **recoverable** — it no
  longer stops the PM loop, matching the precedent set by #2683/#2805's
  post-completion refusal. The cap's actual purpose is preserved by a new
  run-wide `MAX_FAILED_INVOCATIONS = 12` ceiling (4× the per-delegation
  budget): a genuinely failing engineer stays bounded, and only that ceiling —
  a condition no fresh delegation could clear — latches the loop-stopping
  signal. Not a #2805 regression; #2805 fixed the *symptom* and remains correct
  ([#2852](https://github.com/bobmatnyc/trusty-tools/issues/2852))
- **The re-delegation cap was completely silent.** It emitted no log line
  anywhere — run-6's stderr was 5 lines with no mention of it — so the only
  evidence a run had been killed was a label buried in the report's `task`
  field, discoverable only by cross-run forensic comparison. Both the
  per-delegation retry exhaustion and the run-wide ceiling now `warn!` (to
  stderr, never stdout) naming the attempt count and, for the ceiling, that the
  PM loop is being stopped
  ([#2852](https://github.com/bobmatnyc/trusty-tools/issues/2852))
- **`build.rs` re-ran on every single build, in every tree.** Its lone
  `cargo:rerun-if-changed=.git/HEAD` directive was a relative path, which Cargo
  resolves against the *package* root — i.e. `crates/trusty-code/.git/HEAD`,
  which does not exist even in a normal checkout. Cargo treats a missing
  watched path as perpetually changed, so the script never cached. Paths are
  now resolved absolutely via `git rev-parse --git-path` (also making them
  correct in a linked worktree, where `.git` is a *file* and HEAD lives in the
  worktree's gitdir) and emitted only when they exist
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))
- **Guard against a silently stale embedded SHA when the branch ref is packed.**
  Emitting any `rerun-if-changed` opts a script out of Cargo's default
  "re-run when any package file changes" heuristic, so only declared paths are
  watched. With the branch ref packed (`git gc --auto` / `git pack-refs`) no
  loose ref file exists to watch, and a subsequent commit touches neither
  `HEAD`'s content nor any other watched path — so the script would not re-run
  and the embedded SHA/date would go stale until a `cargo clean`, which is the
  very failure this ticket exists to eliminate. The append-only reflog
  (`logs/HEAD`) is now watched as well: it is touched by effectively every
  ref-changing operation regardless of whether the ref is packed or loose,
  closing the gap without reverting to an unconditional re-run
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))

### Changed

- **Deliverable-completeness guidance in the `BASE` preamble (bumped to
  `1.8.0`)** — makes completion of a task's full required-artifact SET robust to
  turn-budget variance on large tasks. On the bake-off L4 task, completion was
  nondeterministic across identical runs: run-2's engineer consumed its entire
  40-turn budget (transcript turns 9..=48) and opened its FINAL turn with "Now
  let me create the README and ARCHITECTURE files", so `ARCHITECTURE.md` was
  never written — while run-3 finished the same task in 29 turns. The provided
  suite passed 9/9 in both, so the code was never the problem: documentation was
  queued LAST, behind ~130 self-authored tests, which made it the only
  deliverable exposed to turn-budget variance. The preamble now tells the model
  to enumerate the required-artifact set as a checklist up front and track what
  remains, treats task-named documentation as a deliverable rather than an
  epilogue, orders required documents at the point the design settles (once the
  code exists and the project's own suite passes) and ahead of any discretionary
  work, forbids starting discretionary work while a required artifact is still
  missing, scales self-authored tests to the stated requirements, points the
  multi-document tail at the `write_files` batch tool (run-2 spent 19 of 40
  turns on one-file-per-turn writes), and requires a final checklist sweep
  before finishing. No turn-budget change: run-2 spent MORE turns than the runs
  that completed, so the budget was not the binding constraint. (#2824)

---

## [0.2.0] — 2026-07-16

### Added

- Surface trusty-search index readiness to daily-driver task sessions: after warming the project's index at task start, `ensure_project_indexed_in_background` now probes `GET /indexes/{id}/status` (via the shared `trusty_common::search_readiness::log_index_readiness`) and emits one stderr line describing which lanes are ready — `warn` while semantic embedding is still warming (expect lexical-only results), `info` once it is ready — so a session is never silently querying a not-yet-ready index ([#2784](https://github.com/bobmatnyc/trusty-tools/issues/2784))
- **Redundant full-suite test re-run suppression (`redundant_run`)** — the
  complement to the `verify_gate` (#2279): where that guarantees the suite runs
  at least once, this stops it running more than needed. When an identical
  `bash` test command already passed and no code has changed since (no
  `write_file`/`write_files`/`edit` and no other shell command in between), the
  agent loop short-circuits the re-run with an explanatory sentinel result
  instead of spawning the suite, so the delegated engineer stops burning turns
  on repeated "one final run to confirm" passes. Opt-in via
  `AgentLoop::with_redundant_run_suppression`, attached at the single
  delegated-engineer construction site alongside the verify gate. The `BASE`
  preamble additionally instructs the model that one clean run after the last
  code change is sufficient and not to repeat identical confirmation runs
  (preamble bumped to `1.7.0`). (#2682)

---

### Changed

- `search_code`: when the semantic (or symbol) lane returns `STAGE_NOT_READY`
  because the vector/KG index stage is still building on a freshly-indexed repo,
  the tool now transparently retries via the always-available lexical lane and
  returns those (degraded) hits instead of an empty "use grep/glob" fallback —
  so conceptual discovery still works during the embedding warm-up window
  (#2783).

### Fixed

- Bound PM re-delegation so a degenerate delegate loop cannot mislabel a
  complete run. Once a delegated engineer reports an explicit successful
  `finish_task` completion, the PM's `delegate_to_agent` tool refuses any
  further re-delegation (nudging it to `finish_task` instead), and
  `run_task`'s report assembler now reports `success`/`no_changes` — never
  `partial`/exit-6 or `deadline_exceeded` — for a run the engineer already
  completed, regardless of how the PM's own loop later terminated. This closes
  the data-integrity bug where a gratuitous post-`finish_task` re-verify round
  that ran out of turns/time corrupted run status and telemetry on a
  fully-passing, all-tests-green run (#2683).

---

## [0.1.0] — 2026-07-09

### Added

- Initial crates.io release.
