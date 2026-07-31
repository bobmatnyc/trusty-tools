Fixed

- delegation-tracker `agentId` presence-check no longer accepts `null`/`""` (closes [#4163](https://github.com/bobmatnyc/trusty-tools/issues/4163))
  - `classify_dispatch` decided "we have an agent id" with key-presence alone
    (`r.get("agentId").is_some()`), while `on_launched`'s consumer rejected a
    non-string value and `on_subagent_stop`'s consumer rejected an empty
    string. A `{"agentId": null}` or `{"agentId": ""}` dispatch response took
    the `Launched` branch and pinned the delegation `Running` with a handle no
    `SubagentStop` can ever quote back, burning the full 6h staleness window as
    a phantom in-flight entry. Both call sites now share one extraction,
    `usable_agent_id` (non-null, non-empty string), so the presence-check and
    the consumers are structurally incapable of disagreeing again.

- managed sessions can reach the bundled agent roster again — every delegation was degrading to `general-purpose` (closes [#4451](https://github.com/bobmatnyc/trusty-tools/issues/4451))
  - Daemon-managed spawns now launch with `--setting-sources user,project,local`
    instead of `project,local`. Claude Code discovers subagents per settings
    tier, and `$CLAUDE_CONFIG_DIR/agents` — where [#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409)
    moved the bundled roster — IS the `user` tier, the one tier the old flag
    dropped. A session showed only the five Claude Code built-ins and answered
    `Agent type 'rust-engineer' not found` while all 42 agent files sat
    correctly on disk.
  - Re-admitting the `user` tier does not re-admit the operator's global
    `~/.claude/settings.json` hooks that [#1269](https://github.com/bobmatnyc/trusty-tools/issues/1269)
    excluded: managed spawns relocate `CLAUDE_CONFIG_DIR`, so the `user` tier
    resolves to the tm-owned config home. Launch paths that do NOT inject
    `CLAUDE_CONFIG_DIR` (`tm launch`, `tm connect`) keep `project,local`
    unchanged — the flag is now selected from that one fact rather than
    hard-coded per call site.
- Abbreviated-subcommand inference (#4398) made the hidden `internal-spawn-disclaimed` shim reachable via any unambiguous prefix (`tm int`, `tm inte`, `tm internal`), which would `posix_spawn` an arbitrary program with macOS TCC responsibility disclaimed. `main` now rejects any invocation whose raw first argv token isn't the exact, full subcommand name before the shim ever runs, while the daemon's own literal self-invocation keeps working unchanged.
- JSON instruction manifest (schema v2) with owner language rules ([#4386](https://github.com/bobmatnyc/trusty-tools/pull/4386)) ([`838d001`](https://github.com/bobmatnyc/trusty-tools/commit/838d00150e4ac1d2f31e5748bf8a85c05b4e78e7))
  - Adds three owner language rules as inline `text` blocks: **Clickable
    References** (`core` — issue/PR/ticket/commit references always render as
    clickable links, never bare numbers), **Banned Word "honest"** (`core` —
    banned from PM responses/delegation briefs/review instructions), and
    **Opportunistic Fixes** (`workflow` — an easy fix found while working on a
    file is made in the same work, never spun into a new issue).
  - Schema v2 adds a `{"kind":"file","path":"…"}` body variant alongside
    `text`/`generated`, resolved through a compile-time `include_str!` table
    (`instruction_pipeline::SECTION_SOURCES`) so a renamed section is a
    compile error.
- reconcile worktrees on a git-derived key, report-only ([#4288](https://github.com/bobmatnyc/trusty-tools/pull/4288)) ([#4382](https://github.com/bobmatnyc/trusty-tools/pull/4382)) ([`7babc82`](https://github.com/bobmatnyc/trusty-tools/commit/7babc82f7353d67327e08626844b91c4eba5187b))
  - Classifies every git-registered worktree `LIVE`/`ORPHANED`/`UNKNOWN` on
    `(git rev-parse --git-common-dir, admin-dir basename)` rather than path
    shape, which was measured misclassifying 34 of 118 worktrees (29%). The
    report also names the **excluded** set (33 of 118, six holding
    uncommitted work) and orders a nested worktree before its parent so
    removing the parent no longer strands the child's registry entry.
    Report-only: no deletions, no sentinel writes, no `--force`.
  - Companion hardening commit for the same issue (test-only, not surfaced by
    the automated changelog): `session_manager::prune`'s
    `active_workspace_paths` parameter is renamed **`in_use_workspace_paths`**
    (rename only — the old name made a `state == Active` filter read as a
    tidy-up rather than the data-loss bug it would be), a pre-existing
    zero-coverage hole in the Phase 2 `fresh_in_use` re-read is closed
    (`phase2_fresh_snapshot_spares_a_record_the_caller_set_missed`), and the
    same "deliberately unfiltered by session state" pin is extended to the
    automatic orphan-GC sweep (`reap_orphaned_worktrees`, which runs on a
    timer with `dry_run: false` hardcoded).
- read named-section PM instruction overrides from CLAUDE.md ([#4324](https://github.com/bobmatnyc/trusty-tools/pull/4324)) ([`f165b92`](https://github.com/bobmatnyc/trusty-tools/commit/f165b92733f9d1cc8656e198359f35d599154b46))
  - Grammar: `<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` … `<!-- TRUSTY-MPM: WORKFLOW END -->`.
    Hosts scanned in precedence order — `CLAUDE.md` then
    `.trusty-mpm/INSTRUCTIONS.md` — with `CLAUDE.md` winning a same-section
    collision. The three floor sections (`identity`, `non-overridable-rules`,
    `framework-guaranteed-conventions`) refuse every override. Nothing here
    fails a launch: a missing/unreadable file, unclosed marker, unknown
    token, or override aimed at the floor all degrade to the bundled section
    with a `warn!`.
- peer message bus foundation — envelope, pub/sub, instance registry ([#4261](https://github.com/bobmatnyc/trusty-tools/pull/4261)) ([`6ffe001`](https://github.com/bobmatnyc/trusty-tools/commit/6ffe001de257ccb4728720d90a11abc8f59a098a))
  - Instance-bypass failure is explicit, never a silent fallback: targeting a
    dead `instance_id` returns `410 Gone` and never redirects to another live
    instance of the same definition; `404`/`409` are separately
    distinguishable.
  - Caller identity is verified, not asserted: the publish path admits only
    `kind: assistant_instance` senders (`400` otherwise), resolves the
    claimed `instance_id` against a live registration (`403
    UnregisteredSender` if not), and re-stamps `definition_id` from the
    registry rather than trusting the caller's copy.
  - Instance ids use `<definition_id>~<8 hex>` (RFC 3986 unreserved `~`, not
    DOC-60 §11's illustrative `#`, which a URI-fragment-aware client would
    truncate).
  - Deferred, not built: the durable inbox and queue-not-spawn behavior (§7),
    cross-project addressing (§12 Q4), retention policy (§12 Q1), and
    version-skew negotiation (§12 Q2/Q5).
- author PM instructions as per-section sources ([#4183](https://github.com/bobmatnyc/trusty-tools/pull/4183)) ([#4264](https://github.com/bobmatnyc/trusty-tools/pull/4264)) ([`c41735d`](https://github.com/bobmatnyc/trusty-tools/commit/c41735dcba886c3518db3b522d2614cdf7ae90a4))
  - Every existing `.trusty-mpm/` override file keeps resolving exactly as
    before. Adds a **Sprint, then Harden** workflow doctrine: sprint to
    feature-complete with targeted tests and no CI iteration loops; harden
    with the full suite, critic, and release gates before publishing; going
    fast never licenses turning red green by deleting coverage; close-and-fold
    at 3+ review rounds.
- compose the bundled-fallback PM prompt via InstructionPackage ([#4183](https://github.com/bobmatnyc/trusty-tools/pull/4183)) ([#4249](https://github.com/bobmatnyc/trusty-tools/pull/4249)) ([`eba04d6`](https://github.com/bobmatnyc/trusty-tools/commit/eba04d67ab940876f55638055a9b195cba5a3ea1))
  - Composed prompt is byte-identical to the previous four-asset-concatenation
    assembly, gated by a test that reports the first differing byte offset on
    failure. Scope is the bundled fallback only — an `AGENT_DELEGATION.md`
    override and `PM_INSTRUCTIONS_DEPLOYED.md` stay on the legacy path by
    design. Adds committed golden-prompt snapshots for both composers
    (regenerate with `UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden`).
- sectioned-JSON instruction package schema ([#4184](https://github.com/bobmatnyc/trusty-tools/pull/4184)) ([#4223](https://github.com/bobmatnyc/trusty-tools/pull/4223)) ([`99a1724`](https://github.com/bobmatnyc/trusty-tools/commit/99a17243866785b0b90f966631ad9d320b12b32c))
  - `validate` gates on `schema_version` before inspecting any field, so a
    package from a later schema reports "unsupported schema_version" instead
    of blaming one of its own valid new keys. Floor blocks are deliberately
    NOT constrained to canonical section order (a faithful lift of
    `BASE_PM.md` places `Trusty Tool Priority` after
    `Framework-Guaranteed Conventions`). A declared section must own at least
    one non-optional block — a section covered only by `optional` blocks
    passed validation and composed to nothing.
- observe native subagent dispatches and give delegations a real lifecycle ([#4096](https://github.com/bobmatnyc/trusty-tools/pull/4096)) ([`6f7530e`](https://github.com/bobmatnyc/trusty-tools/commit/6f7530ef650d4b7dfe1acd5b61b01bcf0c7a0136))
- hard-block git worktree add targeting /tmp, /private/tmp, /var/folders, or the scratchpad ([#3978](https://github.com/bobmatnyc/trusty-tools/pull/3978)) ([`bac23b8`](https://github.com/bobmatnyc/trusty-tools/commit/bac23b8090dfe9d6d5107133dfb7c3dfcdfb8fde))
- **Bare `tm daemon` bypassed the #2486 unsupervised-spawn guard (closes
  [#4397](https://github.com/bobmatnyc/trusty-tools/issues/4397)).** The
  guard against a launchd-managed daemon being duplicated by an unsupervised
  spawn previously existed only on the MCP-bridge path
  (`serve_stdio.rs`'s `no_spawn`); `run_daemon` (the bare-CLI path, invoked
  directly or by a launchd plist) never consulted it, so `tm daemon` run by
  hand walked straight past the same hazard — observed live as PID 35895
  serving a stale 1.2.3 image since 2026-07-29 while the on-disk binary was
  1.3.0. `run_daemon` now refuses to start when a trusty-mpm launchd unit is
  registered but this process is not launchd-supervised, unless the operator
  passes the new `tm daemon --force` opt-in.
- **Decommissioning (or soft-deleting) a session no longer leaves a phantom
  `pending_decision` behind (closes
  [#4400](https://github.com/bobmatnyc/trusty-tools/issues/4400)).**
  `supervisor_status` was reporting decommissioned sessions in
  `pending_decisions` indefinitely — two sessions that never ran sat in the
  human-confirmation queue for 19 days after being torn down. Both terminal
  paths — `decommission_with_root` and `delete_record` — now clear
  `pending_decision`/`proposed_default` unconditionally; `FleetMetrics`
  additionally filters the surfaced list to non-terminal states as defense in
  depth; and the boot-time reconcile sweep backfills (clears) the stale field
  on any terminal record persisted before this fix.
- **An unrelated legacy `.trusty-mpm/` override file no longer shadows every
  CLAUDE.md named-section override (closes
  [#4399](https://github.com/bobmatnyc/trusty-tools/issues/4399)).**
  Previously, if ANY ONE of the four replace-semantics legacy files
  (`PM_INSTRUCTIONS_DEPLOYED.md`, `AGENT_DELEGATION.md`, `WORKFLOW.md`,
  `MEMORY.md`) was present, the whole prompt fell back to the sectionless
  legacy assembly, and every CLAUDE.md named-section override was reported
  "not applied" — including sections the legacy file had nothing to do with
  (an `AGENT_DELEGATION.md` file silently dropped a `WORKFLOW` override
  authored in `CLAUDE.md`). `WORKFLOW`, `MEMORY` and `AGENT-DELEGATION` are
  now independently addressable on that legacy path too: a named override for
  one of those sections lands unless the project's own same-section legacy
  file is what forced the branch, in which case that file still wins,
  unchanged. `IDENTITY`/`CORE`/`SEARCH` have no independent slot in the legacy
  string assembly and remain reported as unapplied, never silently dropped.
- **A corrupted bundled agent is re-deployed instead of frozen forever (closes
  [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408)).** When the
  deployed copy of a bundled agent drifts from the checksum the deploy manifest
  recorded, the file is now refreshed from the bundle rather than classified as
  a user edit and skipped. Previously a degenerate 32-byte stub that replaced
  `rust-engineer.md` in a workspace's `.claude/agents/` was unrecoverable —
  neither a redeploy nor `tm validate --repair` could reach it — so every
  session in that workspace lost the agent from its roster ("Agent type not
  found") permanently. Hand-placed and user-owned agent files are untouched.
- **`tm doctor` no longer reports a serving daemon as unreachable (issue
  #4005).** On 2026-07-26 it printed "trusty-memory unreachable at
  127.0.0.1:7070" while MCP `memory_recall` / `memory_remember` succeeded
  against that same daemon in the same minutes. Two causes, both fixed:
  the probe was single-shot with a 2 s budget, and trusty-memory's `/health`
  samples RSS/CPU behind a mutex and enumerates open file descriptors — work
  the MCP path never touches — so under load `/health` could exceed a budget
  real traffic never approached. The probe now allows 10 s, retries up to 3
  times, and above all distinguishes a TIMEOUT (which proves nothing, so it
  reports `Unknown`) from a REFUSED connection (which proves nothing is
  listening, so it still reports `Fail`). Post-restart warm-up
  (`daemon_state: "warming"`) now reports `Warn` rather than a hard failure.

- **`tm doctor` no longer reports HEALTHY while the memory daemon is wedged
  (issue #4001).** A 2xx from `/health` proves a listener accepted a socket,
  not that the daemon is doing work — which is how doctor stayed green through
  the entire #3992 incident. The memory probe now reads the daemon's own
  worker-occupancy observation out of the response body and fails on a
  reported wedge. A daemon too old to report that block is `Unknown`, not a
  pass: doctor no longer claims health it has not observed.

- **The test suite no longer writes trust-seed entries into the operator's real
  managed `.claude.json`**
  ([#4206](https://github.com/bobmatnyc/trusty-tools/issues/4206)): five tests
  drove the real spawn path (`ClaudeCodeAdapter::spawn`/`spawn_resume` →
  `prepare_managed_config` → `preseed_managed_trust`) without redirecting
  `$HOME`, so each one wrote a `projects.<tempdir>` entry straight into
  `~/.trusty-tools/trusty-mpm/claude-config/.claude.json` — a file that also
  holds `oauthAccount`. 2,443 of 2,538 entries in the reporting operator's
  config were `tempfile::TempDir` paths as a result. Those tests now redirect
  `$HOME` (the isolation seam `dirs::home_dir()` already honours), and a new
  regression test locks the invariant at the layer they all funnel through.
  **No production behaviour changes** — the resolver was never the defect; see
  the PR body for why the originally-reported root cause ("takes no injectable
  base") did not hold up, and why honouring an ambient `CLAUDE_CONFIG_DIR`
  would have made the leak strictly worse.
- **The managed `.claude.json` `projects` map is now self-healing**
  ([#4206](https://github.com/bobmatnyc/trusty-tools/issues/4206)): during the
  read-modify-write `preseed_managed_trust` already performs, entries are dropped
  when — and only when — the directory is DEFINITIVELY absent (a clean
  `NotFound`) **and** the entry carries nothing beyond the four keys the seeder
  itself writes. An ambiguous filesystem error (unmounted volume, down network
  mount, permission denied, I/O fault) always KEEPS the entry, and any entry
  carrying Claude Code's own runtime state (`lastSessionId`, `lastCost`,
  `mcpServers`, …) is preserved even when its path is gone. No new command, no
  background job.
- **A second config quarantine no longer destroys the record of the first**
  ([#4206](https://github.com/bobmatnyc/trusty-tools/issues/4206)): the two
  writers that quarantine a malformed `.claude.json` (`standalone::trust_seed`
  and `core::mcp_config`) both used the fixed name `.claude.json.corrupt`, so
  whichever ran second silently overwrote the first one's evidence. Both now
  share `trusty_common::claude_config::quarantine_path`, which timestamps the
  filename, and both log at `error!` rather than `warn!` — the workspace's
  error-capture layer filters on `!= Level::ERROR`, so the previous `warn!`
  reached no diagnostic surface at all.
- keep the PM persona on in-place relaunch and test the exec seam ([#4340](https://github.com/bobmatnyc/trusty-tools/pull/4340)) ([`714f33e`](https://github.com/bobmatnyc/trusty-tools/commit/714f33ed52f681b6d884a1c380c46935a8f2450a))
  - `compose_inplace_args` omitted `--append-system-prompt-file`; now builds
    the prompt through the same `build_prompt_file` carrier `spawn`/
    `spawn_resume` use. The `std::process::Command` construction is now a
    pure, tested `build_inplace_exec_command` — previously the last step
    before process replacement had zero coverage, which is why this could be
    reported and not refuted from the suite.
- tm ls/picker auto-prunes dead records + groups attached-active-stopped ([#4384](https://github.com/bobmatnyc/trusty-tools/pull/4384)) ([`3dfcb65`](https://github.com/bobmatnyc/trusty-tools/commit/3dfcb6575cd6f3b49929a4dccffacacf3fb52828))
  - Hardened three ways: **record-only removal** (POSTs `/decommission
    ?record_only=true`, so the sweep can never delete filesystem content),
    **two-sightings confirmation + a 5-per-call cap** (a record must read
    `unresumable` on two separate listings before it's auto-decommissioned,
    with the rest reported as "N more pending confirmation"), and a **TTY
    gate** (a scripted/piped/CI invocation of bare `tm` is a pure read, same
    as `tm ls`). `sort_sessions` now groups attached → active → everything
    else above the requested secondary order.
- stop a subagent SessionStart from clobbering claude_session_id ([#4339](https://github.com/bobmatnyc/trusty-tools/pull/4339)) ([`f634d27`](https://github.com/bobmatnyc/trusty-tools/commit/f634d27c9f11f8fb33851ee343f5dc6f26708803))
  - `correlate_session_start` now trusts only the FIRST correlation for a
    record; a later `SessionStart` with a different id is accepted only when
    the stored id's transcript no longer resolves to a live session
    (`runtime::session_id_exists`). `SessionEnd` now clears the field via
    `SessionManager::clear_claude_session_id_if`, and `mark_reactivated`
    clears it outright.
- daemon boot no longer resurrects deleted sessions; decommission no longer force-deletes dirty worktrees ([#4344](https://github.com/bobmatnyc/trusty-tools/pull/4344)) ([`114de33`](https://github.com/bobmatnyc/trusty-tools/commit/114de3331230e16b77d6c6ebd22b056037c07824))
  - `reconcile_on_boot` now asks the record's own `is_terminal()` instead of
    a hand-rolled `Decommissioned`-only check, so `Deleted` records no longer
    fall through to "gone" and get marked `Stopped`. `decommission` reuses
    the #4091 dirty-worktree check (`worktree_safety::inspect_dirt`) before
    removing an SM-created worktree; a dirty candidate is refused and
    reported, never force-deleted. Two follow-ups: the tombstone no longer
    nulls `workspace_path` when nothing was removed, and `tm sessions prune
    --state stopped` reports a distinct `decommissioned_worktree_retained`
    outcome instead of a plain `decommissioned` line either way.
- tm ls cold-latency fix ([`1c54090`](https://github.com/bobmatnyc/trusty-tools/commit/1c5409007f012516fc936e71a4deee1cd2c7972d))
  - Refs [#4322](https://github.com/bobmatnyc/trusty-tools/issues/4322) (does
    not close it — a daemon-state accumulation follow-up remains). `tm ls`
    took 4.9-9.4s cold on a 32-session fleet from blocking filesystem I/O:
    `stale_assets_for_many` ran one `spawn_blocking` with a serial loop, each
    iteration reading ~95 deployed agent/skill files per session. Two
    changes in `daemon::managed_routes::summary`: `Stopped` sessions are no
    longer asset-staleness-probed on the LIST path (56% of the I/O, for a
    marker only actionable at resume — the single-session `GET
    .../managed/{id}` path still probes everything), and the remaining
    per-session comparisons now run concurrently via `JoinSet::spawn_blocking`.
    Measured on that fleet: 3,040 → 1,140 `read_to_string` calls and 35.7 MiB
    → 15.0 MiB per listing. The `tm ls` STATE column also now renders
    `[assets ?]` for a row the daemon did not probe, via a new additive wire
    field `SessionSummary::stale_assets_unchecked`, so the absence of
    `[stale-assets]` can never be misread as "assets fresh".
- make the nested-session guard survive a cold daemon ([#4343](https://github.com/bobmatnyc/trusty-tools/pull/4343)) ([`5f8b338`](https://github.com/bobmatnyc/trusty-tools/commit/5f8b3385f68fbf622b7f2410313357502b8c476a))
  - Fixes [#4335](https://github.com/bobmatnyc/trusty-tools/issues/4335) —
    bare `tm` needing two runs after Claude Code backgrounds. Three changes:
    the managed-list endpoint takes `?slim=true` to skip the `stale_assets`
    probe for callers (folds into #4322 above), the guard's timeout goes
    2s → 8s, and the error arm now logs at debug via `guard_managed_sessions`.
- resume a dead runtime instead of switching into a bare shell ([#4304](https://github.com/bobmatnyc/trusty-tools/pull/4304)) ([`aed8014`](https://github.com/bobmatnyc/trusty-tools/commit/aed80140d4c2179bdf360791b44a218ecf1b2498))
  - `guided_resume::plan_resume` takes a third input, `runtime_live` (via the
    daemon reaper's own `session_has_live_pane` primitive), so a live tmux
    pane whose `claude` process died maps onto the existing
    `ReconcileThenRestart` recovery instead of a plain attach. The verdict is
    ANY-pane-live over the whole tmux session, not the stored `pane_id`
    alone, and fails OPEN throughout. Known limitation: a split session with
    one dead agent pane but another pane running a live non-shell command
    (`vim`, `tail -f`) still reads as live and still attaches into the idle
    pane. Because the reconcile is session-scoped (`kill_session`), the
    restart now discloses it is about to destroy the whole tmux session.
- honour the per-project worktree opt-out in both CLI paths ([#4303](https://github.com/bobmatnyc/trusty-tools/pull/4303)) ([`c11da9c`](https://github.com/bobmatnyc/trusty-tools/commit/c11da9cdaa9dc866698b0e846eb0553b8edde694))
  - `Project.worktree: false` was consulted only inside the daemon's spawn
    route; `tm launch` and the daemon-unreachable bare-`tm` fallback ignored
    it. Both now consult the registry before `ensure_base_clone`, via the new
    `project::worktree_policy` (serves daemon in-process and CLI
    out-of-process off the same `projects.json`). The `unwrap_or(true)`
    default is unchanged.
- serialise projects.json writes across processes ([#4258](https://github.com/bobmatnyc/trusty-tools/pull/4258)) ([`623d6e3`](https://github.com/bobmatnyc/trusty-tools/commit/623d6e3ee7d93bde3e1d942a79d42f60f276e93c))
  - The project-store upsert was an unsynchronised read-modify-write; two
    interleaved writers could silently drop a registration or publish a
    mangled `projects.json` (shared `.tmp` scratch path). All mutations now
    run through `ProjectStore::mutate` under a cross-process file lock,
    published atomically via `trusty_common::json_rmw`. `PATCH
    /api/v1/projects/{name}` gets the same guarantee.
- derive worktrees from git instead of walking five shapes ([#4207](https://github.com/bobmatnyc/trusty-tools/pull/4207)) ([#4224](https://github.com/bobmatnyc/trusty-tools/pull/4224)) ([`69871f1`](https://github.com/bobmatnyc/trusty-tools/commit/69871f13257846edb3109d96d5919dc33b1c851d))
  - Uses `git worktree list --porcelain` instead of five hard-coded location
    shapes. The owning checkout is now resolved with `git rev-parse
    --git-common-dir` instead of a guessed grandparent directory — the old
    guess disowned worktrees physically inside `.base/.worktrees/` but
    registered to the parent repo, making them structurally unreclaimable.
    `tm session prune --worktrees` and the daemon orphan-GC no longer propose
    a plain (non-git-registered) directory. Reclamation is now bounded by a
    structural containment rule — a candidate must be a strict descendant of
    the managed project directory that registered it — since deriving
    discovery from git widened candidates to anything registered anywhere
    under the projects root (on the dogfood machine this admitted five of
    the operator's own long-lived checkouts). A git-`locked` worktree is
    never enumerated for reclamation (the removal path previously treated a
    lock refusal as a generic failure and fell back to deleting the
    directory outright, defeating the lock).
- safe deregister on DELETE /indexes/:id + restore-derived stages on POST /indexes ([#4218](https://github.com/bobmatnyc/trusty-tools/pull/4218)) ([`59674a0`](https://github.com/bobmatnyc/trusty-tools/commit/59674a07d5b8fb74db4374d32dddaf9ae3674921))
- stop two paths writing into a destroyed worktree ([#4204](https://github.com/bobmatnyc/trusty-tools/pull/4204)) ([#4209](https://github.com/bobmatnyc/trusty-tools/pull/4209)) ([`7973aa5`](https://github.com/bobmatnyc/trusty-tools/commit/7973aa5a94c5418ed836578a1621a5e09b1e09a5))
- deploy launch/connect/meta-launch agents into a tier --setting-sources loads ([#4205](https://github.com/bobmatnyc/trusty-tools/pull/4205)) ([`5a68695`](https://github.com/bobmatnyc/trusty-tools/commit/5a686959b360f6b1f12c0e79b6d59fb1b4c2bacd))
- deliver the computed agent roster to the PM prompt ([#4196](https://github.com/bobmatnyc/trusty-tools/pull/4196)) ([`5bf231b`](https://github.com/bobmatnyc/trusty-tools/commit/5bf231b9d085c113bb2b1f48480324023d49475e))
- correct two wrong instructions in bundled assets (index_id, ticket routing) ([`f2b79b3`](https://github.com/bobmatnyc/trusty-tools/commit/f2b79b3add708dd31b9f88b2494aa9891ec49ebc))
- refuse cross-branch git push from managed worktrees ([#2867](https://github.com/bobmatnyc/trusty-tools/pull/2867)) ([#4090](https://github.com/bobmatnyc/trusty-tools/pull/4090)) ([`8d89ca1`](https://github.com/bobmatnyc/trusty-tools/commit/8d89ca1c6d7476a2f7aaa1687c427d48a5dd4513))
- refuse to reclaim a worktree holding unsaved work ([#4091](https://github.com/bobmatnyc/trusty-tools/pull/4091)) ([`dc6bc2a`](https://github.com/bobmatnyc/trusty-tools/commit/dc6bc2a844c7502a03cdaa552f2ab5caddf354d2))
- isolate $HOME in every test that seeds ~/.claude.json ([#4120](https://github.com/bobmatnyc/trusty-tools/pull/4120)) ([`898ec26`](https://github.com/bobmatnyc/trusty-tools/commit/898ec260f397b5915dd8805c6ca2bd2ecd5106bf))
- stream Bedrock via ConverseStream ([#3767](https://github.com/bobmatnyc/trusty-tools/pull/3767)) ([#4112](https://github.com/bobmatnyc/trusty-tools/pull/4112)) ([`49dd042`](https://github.com/bobmatnyc/trusty-tools/commit/49dd042ae5727c1f0e70d8008279e8006e714123))
- stop bare tm from misreporting a managed pane as 'not a git project' ([#4061](https://github.com/bobmatnyc/trusty-tools/pull/4061)) ([#4065](https://github.com/bobmatnyc/trusty-tools/pull/4065)) ([`6c868e9`](https://github.com/bobmatnyc/trusty-tools/commit/6c868e99f1c62bad2078666a31d23aeae0b55a4b))
- serialize concurrent ~/.claude.json trust seeds ([#4075](https://github.com/bobmatnyc/trusty-tools/pull/4075)) ([`707a09d`](https://github.com/bobmatnyc/trusty-tools/commit/707a09dc631cad17a7fbc754f659d8de45a6f1c6))
- emit ChatEvent::Error on failed SSE streams and restore streaming sampling parity ([#3980](https://github.com/bobmatnyc/trusty-tools/pull/3980)) ([`207fa8d`](https://github.com/bobmatnyc/trusty-tools/commit/207fa8de54ddb0361bf6aecc53c079096a5b5f72))
- scan .claude/worktrees for orphans + stop .gitignore fallthrough ([#3974](https://github.com/bobmatnyc/trusty-tools/pull/3974)) ([`cb299bb`](https://github.com/bobmatnyc/trusty-tools/commit/cb299bba75a0c9ff66bbc2719571ce2991482047))
- gate MCP trust-set membership on actual pin success, not the toggle (fifth instance) ([#3951](https://github.com/bobmatnyc/trusty-tools/pull/3951)) ([`58fcca6`](https://github.com/bobmatnyc/trusty-tools/commit/58fcca684a51528dac2801e669f437917e27ee21))
- gate trusty-memory/trusty-search trust on the manifest toggle actually being on ([#3946](https://github.com/bobmatnyc/trusty-tools/pull/3946)) ([`df51741`](https://github.com/bobmatnyc/trusty-tools/commit/df5174178f7084d830e4d23d1ca41bfe0016b471))
- tm launch no longer auto-trusts every committed .mcp.json MCP server ([#3940](https://github.com/bobmatnyc/trusty-tools/pull/3940)) ([`88fc6f4`](https://github.com/bobmatnyc/trusty-tools/commit/88fc6f49301ed3fd83e876021e07b3339a9080d6))
- daemon-managed sessions now trust the project's own trusty-mpm MCP server ([#3924](https://github.com/bobmatnyc/trusty-tools/pull/3924)) ([`385aeff`](https://github.com/bobmatnyc/trusty-tools/commit/385aeff78cf4cac00c3ecd0207ce0d24af2ae0d3))
- pause/resume skills load deferred MCP tool before calling it ([#3919](https://github.com/bobmatnyc/trusty-tools/pull/3919)) ([`39451cf`](https://github.com/bobmatnyc/trusty-tools/commit/39451cf69b8dfef81e7bddb19532f17f09d6c99b))
