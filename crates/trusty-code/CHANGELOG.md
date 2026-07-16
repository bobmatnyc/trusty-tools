# Changelog — trusty-code

All notable changes to trusty-code are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Changed

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
