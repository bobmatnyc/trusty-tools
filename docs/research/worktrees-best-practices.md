# Worktree lifecycle and build efficiency

Date: 2026-09-21  
Status: Research and recommendations; not an adopted specification  
Scope: trusty-mpm worktree creation, ownership, handoff, retention, and removal on a shared development machine  
Source checkout inspected: `026123ff40b7554978cf86f6b6aada14ca84008c`

## Recommendation

Manage source worktrees, cached build output, and permission to compile as
three separate resources. Give each a clear owner and an independent lifetime.

Keep a worktree attached to a logical task across sequential agent handoffs,
preserve useful build state while that task remains active, and acquire build
capacity when compilation actually begins. Reclaim completed source checkouts
after checking their contents and ownership; manage retained build caches by
disk budget and reuse value.

This is a refinement of work already underway, not a proposal to start another
build scheduler. The existing [dynamic build slots research](dynamic-build-slots-2026-09-20.md)
covers persistent output directories and capacity-based admission under #8261.
The owner confirmed in this investigation that excluding Python jobs from
builder slots is already being implemented. That exclusion is not presented
here as a new finding requiring another implementation effort.

## Three layers

Added by the PM session on 2026-09-21, on the owner's ruling. This work spans
three layers: a framework rule must hold for a repository in any language, a
language adapter knows one build tool, and a project section records
decisions about one codebase. Separating them keeps a framework rule from
being written for Rust alone and keeps a project decision out of the shared
framework rules.

| Layer | What belongs here | Tracked in |
| --- | --- | --- |
| Framework | Worktree lifecycle (create, work, handoff, pause, finish, remove, abandon); fixed-path pool lease decided at dispatch through the `WorktreeCreate`/`WorktreeRemove` hooks; one writer per tree, sequential reuse; reconciliation of trees that end without a merge; count cap and a doctor report by disposition; guard refusal text; a worktree skill for the `version-control` agent; the opaque build-state slot a language adapter fills | [#8343](https://github.com/bobmatnyc/trusty-tools/issues/8343), with [#7914](https://github.com/bobmatnyc/trusty-tools/issues/7914), [#7771](https://github.com/bobmatnyc/trusty-tools/issues/7771), [#7889](https://github.com/bobmatnyc/trusty-tools/issues/7889) |
| Language (Rust) | Target-directory slots and admission; stable source path because cargo keys path crates on it; job budget per admitted build; dev-profile debug settings; sccache and `SCCACHE_BASEDIRS`; `cargo-hakari`; `cargo nextest archive`; the bundled `rust-build-performance` skill | [#8261](https://github.com/bobmatnyc/trusty-tools/issues/8261), [#8329](https://github.com/bobmatnyc/trusty-tools/issues/8329), [#6868](https://github.com/bobmatnyc/trusty-tools/issues/6868), and the Rust-adapter baseline, [#8344](https://github.com/bobmatnyc/trusty-tools/issues/8344) |
| Project (trusty-tools) | Test-only dependency edges between workspace crates and the `cargo metadata` guard; trusty-mpm's 53 integration-test binaries; dev edges that re-declare trusty-common with extra features | [#8341](https://github.com/bobmatnyc/trusty-tools/issues/8341), and the trusty-mpm test layout, [#8345](https://github.com/bobmatnyc/trusty-tools/issues/8345) |

The milestone `1.6.5 · Build efficiency` is organised by these layers.

## Why this matters

Worktrees provide separate source directories and indexes at low Git storage
cost because they share the repository's objects. Their dependency downloads,
generated files, and compiled output can cost much more than the checkout.
Creating a fresh worktree and cold build directory for each agent retry can
therefore turn safe source isolation into repeated compilation.

Conversely, sharing one target directory across every concurrent task creates
Cargo lock contention. Retaining every old target directory indefinitely trades
rebuild time for unbounded disk consumption. The policy must balance these costs
without risking unfinished work.

## Evidence from the current machine

The following is a point-in-time investigation on September 21, approximately
11:34–11:40 America/New_York. It is not a controlled build benchmark.

| Observation | Interpretation and limit |
| --- | --- |
| Mac Studio, M4 Max, 16 CPU cores, 128 GB RAM | Already substantial Rust build hardware; replacement value is unproven. |
| `~/.cargo/config.toml` sets `build.jobs = 4` | Builds inherit four jobs unless overridden. This is conservative for one build, but may be reasonable for several simultaneous builds. Jobs are not a strict CPU-core utilization limit. |
| Three sampled agent worktrees had no checkout-level Cargo configuration; recent successful command dispatches used separate explicit target directories | The main checkout's shared target configuration was not automatically providing those worktrees with warm artifacts. |
| Recent check logs contained extensive dependency compilation/checking; two commands exceeded the tool's 600-second foreground timeout | There is meaningful build work and delay, but these logs alone do not attribute elapsed time between compilation, contention, or resource pressure. |
| `sccache --show-stats` returned zero requests and a 10 GiB maximum; Cargo configuration declares a 50 GiB cache size | Cache reuse and effective server configuration need verification during a real build. Zero counters do not establish historical cache effectiveness. |
| CPU was about 50–65% idle during sampling; swap usage was zero; no thermal warning was reported | No Rust compiler was running in these samples. They cannot establish active-build saturation or exclude earlier memory pressure. |
| One `libgit2-sys` custom build process exited with SIGKILL | Work failed; neither the sender nor the reason for the kill was established. Do not label this an out-of-memory event without additional evidence. |
| Session records contained Rust dispatch denials while Python engineers occupied builder slots | Admission delay was demonstrated. The owner identified the Python exclusion as work already in progress. |
| The live daemon reported version 1.6.4, cap 9, and three held builder leases while no Cargo/rustc processes were visible | Agent lease occupancy and active compilation were different at that instant; this does not mean all leases were stale. |

Follow-up, 2026-09-21 afternoon (PM session tm-dogfood): a later sccache reading supersedes the row above without changing it. See "Follow-up findings, 2026-09-21 afternoon (PM session tm-dogfood)" below.

Evidence was read from system process/resource snapshots, local Cargo settings,
the daemon's `/health` and `/api/v1/builder-slots` endpoints, and recent agent
transcripts/build logs. Relevant sessions were
`8ccd3824-e93c-43ee-a94e-3c9148ec0433` and
`60975f66-3e73-4817-befd-9c9a4ccf2d18`.
No builds were launched, settings changed, or sessions interrupted.

## Follow-up findings, 2026-09-21 afternoon (PM session tm-dogfood)

These findings come from read-only research agents dispatched by the PM
session on 2026-09-21 in the afternoon, America/New_York timezone, unless a
step names its own build. No builds were launched and no settings were
changed except where a finding says otherwise.

1. **[Language: Rust] sccache, superseding the "zero requests and a 10 GiB maximum" row
   above.** A later `sccache --show-stats` reading reported a 50 GiB maximum
   cache size, 24 GiB used, 91 compile requests, 0 cache hits, 48 misses, 4
   cache timeouts, and 29 non-cacheable calls (20 for `crate-type`, 5 for
   `missing input`). `~/.cargo/config.toml` sets `build.rustc-wrapper =
   "sccache"` machine-wide and `[env] SCCACHE_CACHE_SIZE = "53687091200"`.
   sccache applies its cache-size setting when its server starts, so the
   earlier 10 GiB reading is consistent with a server that started before
   the setting reached it; this was not confirmed. sccache runs outside
   launchd. tm's own `[build] sccache` flag is absent from
   `~/.trusty-mpm/config.toml`, so the brief line tm generates in
   `crates/trusty-mpm/src/core/build_env.rs` (`paste_line`) omits
   `RUSTC_WRAPPER`; builds still route through sccache because of the global
   Cargo setting. `crates/trusty-mpm/src/core/build_env_repair.rs`
   deliberately refuses to write that global key. Conclusion, with its
   limit: sccache sits in the build path and is sized as intended; a zero
   hit rate over 91 requests is current, and it says nothing about earlier
   server lifetimes.

2. **[Language: Rust] Why hits are zero across worktrees.** Two agent target directories on
   the same branch family, `target-8261-r4` and `target-8261-r5`, were
   compared. The `.fingerprint` entry for a registry crate,
   `serde-13aa31014ad21d36`, is identical in both directories. The entries
   for a workspace crate, `trusty-common-*`, differ across all five sampled
   entries. Cargo fingerprints a path crate using its absolute path as an
   input. Neither `~/.cargo/config.toml` nor the repository's
   `.cargo/config.toml` sets `--remap-path-prefix` or `build.rustflags`.
   Whether `--remap-path-prefix` would produce sccache hits across paths
   is now resolved: it does not, on its own, because sccache hashes
   compiler arguments verbatim and the flag's own value carries the
   differing absolute path. The lever sccache provides instead is
   `SCCACHE_BASEDIRS`, added in 0.14
   ([mozilla/sccache#2521](https://github.com/mozilla/sccache/issues/2521)),
   which normalises paths under configured base directories before
   hashing. The installed version is 0.17.0, which carries it.
   [mozilla/sccache#2863](https://github.com/mozilla/sccache/issues/2863)
   reports a wrong-code risk from combining base directories with
   preprocessor cache mode on 0.18.0 and later, so pin the version before
   adopting it. The #8261 design work already measured sccache as neutral
   on this workload, dominated by path crates and incremental artifacts
   sccache cannot cache, so this stays low priority. It matches the
   [#8261](https://github.com/bobmatnyc/trusty-tools/issues/8261) finding
   that an APFS clone of a target directory keeps registry dependencies
   warm only.

3. **[Framework] Disk and count.** `git worktree list` showed 33 entries. Nine target
   directories exist under `.claude/worktrees/*`, ranging from empty to
   30 GiB (`agent-aaa174af562438c1e/target-8261-r4`), with several between
   700 MB and 6.6 GiB; all nine were modified within 48 hours. `tm doctor`
   reports tm's derived shared target directory as
   `~/.trusty-tools/cargo-target/bobmatnyc/trusty-tools`. The agent trees
   were not using it, consistent with
   [#8261](https://github.com/bobmatnyc/trusty-tools/issues/8261)'s interim
   rule that the PM assigns a distinct target directory per building agent.
   A full per-tree inventory with dispositions is in progress and is not in
   this document yet.

4. **[Language: Rust] Observed build durations today, under load, not benchmarks.** A
   `cargo install --path crates/trusty-memory --locked` release build from
   a fresh detached worktree took 103 minutes 7 seconds while two other
   cargo builds shared the host. A `cargo test -p trusty-mpm` test-profile
   build in a fresh worktree, for PR
   [#8334](https://github.com/bobmatnyc/trusty-tools/pull/8334) round 5,
   was still compiling dependencies after more than two hours. The previous
   PM session recorded 45 to 120 minutes for that same build. `build.jobs =
   4` applied to all of these runs.

5. **[Project] Test-only dependency edges weld crate compile graphs together
   ([#8341](https://github.com/bobmatnyc/trusty-tools/issues/8341)).** The
   owner's stated principle: separate crates exist for a more efficient
   compilation process. A metadata-only audit, using `cargo metadata` and
   `cargo tree` with no build, examined every workspace-to-workspace
   dev/build edge. It found five instances where a workspace crate absent
   from the consumer's normal tree is pulled in for one test file.

   | Consumer | Test-only edge | Workspace+registry crates uniquely added | Used by |
   | --- | --- | --- | --- |
   | trusty-code-gui | trusty-code | 198 | one `#[cfg(test)]` block in `src/bridge/mod.rs` |
   | trusty-analyze | tga, trusty-audit, trusty-installer (trusty-console in comments only) | about 135 combined | `tests/uds_consumer_contract.rs` |
   | trusty-memory | trusty-installer | 13 | `tests/uds_consumer_contract.rs` |
   | trusty-mpm | trusty-review | 8 (four AWS SDK crates, redb, hmac, arc-swap, trusty-review) | `tests/conformance_cross_gate.rs` |
   | trusty-review | trusty-console, trusty-installer | not measured | no `use` site found |

   Crate counts are set differences between dependency trees, not compile
   time. About 97% of trusty-review's tree already sits inside
   trusty-mpm's normal tree; that edge is the smallest of the five, and
   removing it will not by itself explain the trusty-mpm build time. No
   dev-dependency cycles were found. Dev edges that re-declare a normal
   dependency with extra features, seen in six crates for trusty-common,
   add no crate to the graph; under resolver v2 one `cargo test` run
   compiles the union once, and the double compile appears only when
   `cargo build` and `cargo test` alternate in one target directory. A fix
   and a `cargo metadata` CI guard are in progress on branch
   `fix/8341-test-only-crate-edges`; the rule is added to the bundled
   `rust-build-performance` skill on branch `docs/8341-crate-edge-rule`.
   The no-build check is `cargo tree -p <crate> -e dev --prefix none`; it
   should add no workspace crate that `-e normal` lacks.

6. **[Project] Integration-test binaries.** Each top-level file under a crate's
   `tests/` directory compiles as its own crate and links the whole
   library. Counts at the inspected checkout: trusty-mpm 53, trusty-search
   32, trusty-memory 31, trusty-common 14, trusty-review 11, trusty-analyze
   8. One unmeasured hypothesis: the link steps take a material share of
   the trusty-mpm test-profile build. Consolidating into one
   `tests/it/main.rs` per crate is the commonly cited remedy. Tests that
   mutate process environment or global state, and `serial_test` is in use
   here, may not merge safely. This research is in progress.

7. **[Framework] Why worktrees accumulate: causes confirmed today by hitting them.**
   - The harness removes an isolated agent's worktree only when the agent
     leaves it unchanged; any tree that received a commit stays.
   - `tm hook --pm-guard` refuses worktree removal by every role except
     `version-control`
     ([#5791](https://github.com/bobmatnyc/trusty-tools/issues/5791),
     ADR-0057), including an agent removing the empty tree it created
     itself. A `local-ops` agent could not remove its own detached install
     tree.
   - A `version-control` removal of that same tree succeeded on retry.
     Writing the path out in full, plain `git worktree remove` removed it,
     with no `--force`. The tree was detached at `origin/main`, held no
     local-only commits, and its untracked `target-install/` directory is
     ignored by `.gitignore:8` (`**/target-*/`), so the tree was clean.
     A rule admitting a clean, fully-pushed detached tree exists in commit
     `f73dc9300`, proven by the test
     `a_detached_head_holding_no_local_only_commit_is_reclaimable` in
     `crates/trusty-mpm/src/bin/tm/commands/pm_guard_bash/worktree_remove.rs`,
     and the rule is in the installed build.
   - The refusal the retry reproduced came from the guard's first, lexical
     re-check, `worktree-scope`
     (`crates/trusty-mpm/src/bin/tm/commands/pm_guard_bash/worktree_remove.rs`,
     the unresolved-target branch): the command held the path in a shell
     variable, and the guard expands only `$TMPDIR`, `$TMP`, `$HOME`,
     `$PWD`, and a leading `~`. The cause of the first refusal was never
     established, because that agent paraphrased the hook text instead of
     pasting it.
   - Real usability gaps remain: (i) every refusal appends the full list of
     conditions after naming the failed check, so agents misreport which
     check failed — this cost two dispatches; (ii) a path held in an
     ordinary shell variable is refused; (iii) every refusal recommends `tm
     session prune-worktrees --merged-prs --force`, a forced sweep the
     previous PM session recorded as fleet-wide, even when the remedy is
     retyping the path.
   - The delivery sequence's last step, "remove worktree, then delete
     branch" in `docs/reference/worktree-discipline.md`, depends on a
     dispatch after every merge; nothing performs it automatically.

8. **[Framework; the target-directory pairing is the Rust adapter's part] Proposed refinement to this document's lifecycle: supported, with a
   known integration point and one unverified detail.** This document
   already says to keep a worktree attached to a logical task across
   handoffs. The PM session's addition: Cargo keys a workspace crate on its
   absolute source path, so the reusable unit is a fixed-path worktree
   paired with its own target directory. That pairing would be leased and
   returned like a [#8261](https://github.com/bobmatnyc/trusty-tools/issues/8261)
   build slot, and re-pointed with `git switch`, which changes only the
   files that differ between branches. A persistent target directory
   detached from a stable source path keeps registry dependencies warm
   only, per finding 2 above. Reuse would be sequential: one writer per
   tree at a time. Two concurrent writers in one tree share a Git index,
   which risks lock races and interleaved commits; they also share compile
   state, so one writer's half-edited crate can break the other's build;
   they share the target-directory lock; and they share one branch and
   therefore one pull request. Deliberate batching, done by one agent or
   agents in sequence, is the exception to that rule. Two supporting ideas:
   affinity, leasing the tree that last built the crate under change, and
   pre-warming an idle tree at `origin/main` after each merge.

   Measurement already recorded in the #8261 design work ([dynamic build
   slots research](dynamic-build-slots-2026-09-20.md)) for one build under
   three conditions: fresh worktree with cold target directory 200 s; warm
   target directory reached from a different source path 103 s; warm
   target directory at the same source path 17 s. Limit: this document's
   authors did not re-run it, and the exact command is recorded in that
   document, not here — command not re-verified. Reading: a persistent
   target directory recovers about half of the cold cost, and a stable
   source path recovers most of the remainder. Both are needed; only the
   first exists today.

   Cargo mechanism, confirmed from cargo's fingerprint documentation:
   freshness compares source mtimes against a per-unit dep-info anchor, and
   `git switch` rewrites only blobs that differ, so a fixed-path tree
   rebuilds only crates whose files changed between branches.
   `-Zchecksum-freshness`
   ([rust-lang/cargo#14136](https://github.com/rust-lang/cargo/issues/14136))
   would replace mtime with size plus checksum but is nightly-only, so
   unusable at MSRV 1.94. Unmeasured pitfalls: `Cargo.lock` churn on
   switch, switching to an old base, and stale artifacts growing a
   long-lived target directory — no `cargo sweep` or `cargo clean -p`
   policy exists in the repo.

   What blocks a pool today: the harness, not tm, creates the tree for an
   isolated dispatch (`docs/reference/worktree-discipline.md`;
   `crates/trusty-mpm/src/bin/tm/commands/pm_guard_worktree_grant.rs`
   states trusty-mpm "still creates nothing"). Re-pointing a live agent is
   unsafe: `pm_guard_enter_worktree.rs` documents
   [#7172](https://github.com/bobmatnyc/trusty-tools/issues/7172), where
   `EnterWorktree` moved the cwd while the harness's isolation pin stayed
   at the dispatch-time path, and tm now denies the switch. So a lease
   must be decided at dispatch.

   Integration point: Claude Code documents a `WorktreeCreate` hook
   ([hooks](https://code.claude.com/docs/en/hooks),
   [worktrees](https://code.claude.com/docs/en/worktrees#customize-worktree-creation))
   that fires for `--worktree`, `isolation: "worktree"`, and background
   sessions, replaces the default worktree logic, and returns the
   directory to adopt; a `WorktreeRemove` hook pairs with it. ADR-0036
   (2026-08-09) says the harness exposes no way to relocate worktrees; that
   was true of settings and is now stale with respect to hooks. Unverified:
   whether the hook's input JSON carries the requesting agent type or
   branch, or only a generated name. An instrumented dry run is needed
   before design.

   Ownership: the tm daemon extends the #8261 slot lease to pair a
   target-directory slot with a stable-path worktree slot under one
   lifecycle; the `WorktreeCreate`/`WorktreeRemove` hook is a new tm-owned
   artifact; the `version-control` agent gets a skill for pool health and
   reset, invoked through tm, with no new authority; the PM brief names a
   branch and never a path; a worktree-count cap belongs in the daemon's
   worktree doctor check, none exists today.

9. **[Project] Integration-test consolidation carries real risk.** Of eight sampled
   files among trusty-mpm's 53, `tests/scratch_home_tmux_gate.rs` and
   `tests/test_session_lifecycle.rs` mutate the process-wide `$HOME`
   through `unsafe std::env::set_var` inside a restore-on-drop guard. That
   is safe today only because each file is its own OS process. In one
   merged binary the default thread-parallel runner would race them, so
   they would need `serial_test` or a single-threaded subset. A shared
   `tests/common/mod.rs` already exists. Measure first: `cargo build
   --timings -p trusty-mpm --tests` link-step total and `cargo test -p
   trusty-mpm --no-run` wall time, on an idle host.

10. **[Language: Rust] Outside practice compared with this repository.**

    | Practice | Cost it removes | Present here | Effort |
    | --- | --- | --- | --- |
    | [Framework] Fixed multi-checkout pool with per-slot target directory | Cold checkout and cold target | Target-directory half exists via #8261, worktree half does not | Medium |
    | `cargo nextest archive` | Rebuilds when tests run elsewhere | No `.config/nextest.toml` | Low to medium |
    | `cargo-hakari` workspace-hack | Duplicate rebuilds of shared dependencies when different `-p` selections resolve different feature sets | Absent | Medium |
    | `debug = "line-tables-only"` and split debuginfo in the dev profile | Link time | `profile.dev` sets only `opt-level = 0` | Low |
    | Remote or distributed build systems (Bazel, buck2) | Out of scope, not evaluated | — | High, likely disproportionate |

11. **[Framework] Worktree inventory, 2026-09-21 afternoon, 33 entries after `git
    fetch`.**

    | Category | Count |
    | --- | --- |
    | Live with agents working | 7 (main checkout, the #8334 round-4 tree, two round-5 trees, two #8341 trees, and a temporary install tree removed later that day) |
    | Open work | 8 (open PRs [#8322](https://github.com/bobmatnyc/trusty-tools/pull/8322), [#8312](https://github.com/bobmatnyc/trusty-tools/pull/8312), [#8315](https://github.com/bobmatnyc/trusty-tools/pull/8315), [#8317](https://github.com/bobmatnyc/trusty-tools/pull/8317), plus unpushed branches for issues [#8282](https://github.com/bobmatnyc/trusty-tools/issues/8282), [#8269](https://github.com/bobmatnyc/trusty-tools/issues/8269), [#8187](https://github.com/bobmatnyc/trusty-tools/issues/8187), [#8236](https://github.com/bobmatnyc/trusty-tools/issues/8236)) |
    | Merged and clean | 4 |
    | Empty detached duplicates of another branch's tip | 3 |
    | Needs an owner | 11 |

    The 11: four with uncommitted or unpushed work and no PR
    (`fix/7889-worktree-reclaim` 8 files, `docs/8133-8309` 5 files,
    `fix/8297-builder-cap` 3 files, `fix/8205-followup` unpushed); four
    superseded predecessors of a later round; two detached commits on no
    branch (`abb1027f`, `64165c46`); one external tree on
    `codex/search-context-exp`.

    In-tree target directories total about 43 GB across 8 trees, 30 GB of
    it in the live round-4 tree; the seven removable trees free about
    5 GB. A `du` of the shared out-of-tree target directory did not finish
    in 60 s and is not counted. 23 `worktree-agent-*` branches have no
    tree; three are unmerged, and one of those
    (`worktree-agent-ac05093c…`) has no backup ref. About 26 `backup/*`
    refs have no owner. `git worktree prune --dry-run` reports nothing:
    every entry is a real directory.

    The delivery sequence assigns removal after a merge, and assigns
    nobody for the other endings — a branch that never gets a PR, one
    superseded by a later round, a tree left dirty. The harness cleans
    only unchanged trees, the merged-PR sweep skips anything without a
    merged PR, and nothing reconciles on a schedule. This matches the
    original document's Abandon lifecycle stage and its "Explain every
    skipped cleanup" bullet, both of which describe this missing step.

## Language adapters

Added by the PM session, 2026-09-21 afternoon. The framework lease carries an
opaque build-state slot; a language adapter fills it. Every adapter answers
the same five questions, which are the sub-headings below. Rust is the only
adapter researched so far.

### Rust

#### Build output location
Output lives in `CARGO_TARGET_DIR`, and it is relocatable outside the source
tree; #8261 pairs one target-directory slot per pool slot.

#### Freshness key
Cargo compares source mtimes against a per-unit dep-info anchor, and it keys
a path crate on that crate's absolute source path, so the source path is part
of the freshness key (finding 2). Findings 2 and 8 measured this: 200 s cold,
103 s warm from a different source path, 17 s warm at the same source path.

#### Shareable cache
sccache can share non-incremental compiler output across directories only
with `SCCACHE_BASEDIRS` set, and #8261's design work measured sccache as
neutral on this workload (findings 1 and 2).

#### Pre-warm command
A test-profile `cargo test --no-run` for the crates most often changed, run
at `origin/main`, is the candidate pre-warm command for an idle pool tree
after a merge; this is unmeasured.

#### Eviction
Only regenerable build output is safe to delete, bounded by disk budget and
last use; no `cargo sweep` policy exists yet (finding 8, and "Evict build
output independently" above).

### Go

#### Build output location
Not researched.

#### Freshness key
Not researched.

#### Shareable cache
Not researched.

#### Pre-warm command
Not researched.

#### Eviction
Not researched.

### Swift

The repository's line-cap check already scans Swift sources.

#### Build output location
Not researched.

#### Freshness key
Not researched.

#### Shareable cache
Not researched.

#### Pre-warm command
Not researched.

#### Eviction
Not researched.

### TypeScript and JavaScript

This repository's Svelte UIs build under pnpm, and an untracked
`.pnpm-store/` directory in the main checkout on 2026-09-21 is the same kind
of per-tree cost.

#### Build output location
Not researched.

#### Freshness key
Not researched.

#### Shareable cache
Not researched.

#### Pre-warm command
Not researched.

#### Eviction
Not researched.

### JVM (Java and Kotlin)

#### Build output location
Not researched.

#### Freshness key
Not researched.

#### Shareable cache
Not researched.

#### Pre-warm command
Not researched.

#### Eviction
Not researched.

### .NET

#### Build output location
Not researched.

#### Freshness key
Not researched.

#### Shareable cache
Not researched.

#### Pre-warm command
Not researched.

#### Eviction
Not researched.

### C and C++

#### Build output location
Not researched.

#### Freshness key
Not researched.

#### Shareable cache
Not researched.

#### Pre-warm command
Not researched.

#### Eviction
Not researched.

## Proposed lifecycle

| Stage | Practice | Required evidence or record |
| --- | --- | --- |
| Create | Fetch the intended upstream and create the branch/worktree from an explicit revision. Use a sibling directory, not a checkout nested inside another task's worktree. | Task ID, owner/session, repository identity, branch, base commit, absolute path, parent task if applicable. |
| Work | Assign one writer. Independent writers get separate worktrees. A reader can inspect an existing tree, but a reproducible review requires a pinned revision or coordination that prevents changes during review. | Current owner and active operations. |
| Handoff | Transfer the same task's tree to a successor only after the previous writer relinquishes it. Preserve its build state when useful. | Recorded transfer, HEAD, dirty/untracked status, unfinished operations, next action. |
| Pause | Keep resumable source work and release build capacity after the build exits. Do not equate an agent waiting for review with an active compiler. | Resume record and retention reason; confirmed build completion before releasing its exclusive output-directory lease. |
| Finish | Confirm delivery and preserve review/test evidence and useful outputs. Check for changes made after the PR merged. | PR disposition, current HEAD, unpushed commits, dirty/untracked files, live users/processes, required artifact locations. |
| Remove | Let the owning manager remove a verified disposable worktree, then delete the branch when safe. | Explicit eligibility result; any uncertainty leaves the tree intact. |
| Abandon | Preserve valuable work under a durable reference or archive before removal, or obtain explicit authorization to discard it. | Abandonment decision and preservation location. Age alone is insufficient. |

An agent exiting is an ownership event, not sufficient proof that its worktree
can be deleted. A parent task ending likewise does not make unsaved child work
disposable. These are proposed safety requirements for lifecycle integration;
they should be reconciled with the child-reclamation language in
[DOC-66](../specs/DOC-66-session-workstream-model.md), rather than silently
overriding it.

## Build output and admission

### Preserve useful reuse without merging ownership

Cargo retains incremental artifacts in its build directories. Preserve those
directories through a task's edit/test/review cycle; avoid unconditional
`cargo clean`, toolchain changes, and unnecessary feature/profile changes.
An agent replacement should not itself invalidate the cache.

For concurrent builds, use independently leased target directories. The
existing #8261 design's persistent pool is a candidate mechanism. Its reuse
benefit must be measured: moving between source paths can invalidate some
artifacts, and a persistent directory does not guarantee a warm build.

A pool needs more than mutual exclusion around the Cargo process:

- Record repository, toolchain, target architecture, profile/features, and
  relevant build flags so compatibility and invalidation are understandable.
- Keep exclusive ownership through any direct execution or verification of
  mutable binaries in that directory, or copy outputs to an immutable,
  provenance-labelled artifact location before releasing it.
- Bind verification evidence to the source revision and build configuration.
- Preserve the existing workflow's special handling of tools such as the
  SemVer gate; a shared target directory must not be injected indiscriminately.
- Recover leases using verified process identity and state. Elapsed time alone
  must not authorize deleting output beneath a live build.

Cargo's compiler cache and incremental compilation are distinct. The repository's
[Rust build-performance skill](../../.agents/skills/rust-build-performance/SKILL.md)
documents that sccache cannot cache incremental compiler output. Measure cache
hits for the actual workload before changing incremental settings globally.

### Budget compilation across the machine

Count and limit actual build activity separately from open tasks and worktrees.
Coordinate per-build job counts with total admitted builds: raising every task
to twelve jobs could overcommit the same host even if twelve helps a single build.
Native build scripts, linking, tests, and other projects also consume resources;
Cargo job counts are only part of the resource budget.

### Evict build output independently

Use a configurable disk budget and last-use information to evict inactive cache
entries. Verify there is no live build or artifact consumer. Delete only known
regenerable build output, not arbitrary ignored/untracked files.

Removing an old cache should leave the source task resumable. Removing a finished
worktree need not evict a separately managed reusable cache. Neither action should
silently change the other's lifecycle state.

## Safe disposition details

- **Use managed removal.** Git's `worktree remove` handles the checkout and its
  registration. `worktree prune` removes stale administrative entries; it is
  not a cleanup policy for existing old directories. Under trusty-mpm, follow
  the owning manager's cleanup path rather than having individual agents remove
  each other's worktrees.
- **Check contents after merge.** A merged PR can coexist with later uncommitted,
  untracked, or unpushed work. Preserve valuable ignored files as well.
- **Account for squash merges.** Git ancestry alone may not recognize that a
  feature branch's changes landed. Confirm the PR disposition and whether its
  reviewed head corresponds to the work being reclaimed.
- **Treat locks as protection, not proof of activity.** Git worktree locks protect
  against removal/pruning; they are not writer mutexes or process heartbeats.
- **Remember shared Git state.** Worktrees have separate indexes and HEADs but
  share refs and normally repository configuration. The stash is repository-wide.
  Avoid using a global stash to move another session's work out of the way.
- **Explain every skipped cleanup.** Unknown owner, active process, dirty tree,
  unique commit, retained artifact, and explicit retention should be distinct
  reasons, not an undifferentiated failure.

## Validation before changing defaults

Run a controlled experiment after the current admission changes are installed
and verified. Pin one source commit, toolchain, feature set, profile, and command.
Use isolated benchmark directories; never clean another session's cache.

| Experiment | Question |
| --- | --- |
| Equivalent cold builds at 4, 8, and 12 jobs | Does a single build materially benefit from more concurrency? |
| Repeated warm builds, including a representative source edit | How much does retaining a task's build state save? |
| Same revision in a fresh source worktree with a reused pool directory | How much reuse survives source-path changes? |
| Two simultaneous builds in separate leased directories | What job allocation improves total throughput without making the host unresponsive? |
| Compiler-cache statistics before and after each run | Is sccache contributing, and which calls are non-cacheable? |
| Fixed-path tree re-pointed with `git switch` between two branches near `origin/main`, versus a fresh tree on the second branch | How much does a stable source path save? |
| The same commit built from two directories with `SCCACHE_BASEDIRS=<common root>` set, comparing `sccache -s` hit counters before and after | Does `SCCACHE_BASEDIRS` yield cross-directory hits? |
| `cargo test -p trusty-mpm --no-run` wall time and `cargo build --timings` before and after consolidating integration tests into one binary | What share of the build is link time? |
| `cargo test -p <consumer> --no-run` before and after removing the test-only edges in finding 5 above | How much does removing a test-only edge shrink the dependent's test build? |

The four rows above this line were added by the PM session on 2026-09-21 afternoon; see "Follow-up findings, 2026-09-21 afternoon (PM session tm-dogfood)" for the observations behind them.

Control cache warmth across comparisons; do not compare an initial cold run
against a later warm run and attribute the difference to job count. Repeat or
alternate run order enough to distinguish background-load variation.

Record end-to-end latency, queue/lock wait, Cargo timing reports, CPU utilization,
peak memory/pressure, swap activity, cache hits/misses, and disk footprint. Record
concurrent non-benchmark activity. Evaluate both individual completion time and
total completed work per unit time.

No speedup estimate or optimal concurrency setting is established by this report.
The evidence supports measuring and improving lifecycle/cache use before buying
replacement hardware.

## Build-time plan for 1.6.5

Added by the PM session, 2026-09-21 afternoon.

1. **[Language: Rust] Baseline on an idle host.** Run `cargo test -p
   trusty-mpm --no-run` cold and warm with `--timings`. Unmeasured.
2. **[Language: Rust] Job budget.** The machine-global cargo config sets 4
   jobs on 16 cores, and every agent build inherited it. Derive a budget from
   cores and admitted builds and hand it out with the slot lease; tm does not
   write the global cargo config. Unmeasured.
3. **[Project] Remove test-only crate edges,
   [#8341](https://github.com/bobmatnyc/trusty-tools/issues/8341).**
   Unmeasured.
4. **[Framework + Language: Rust] Land build slots,
   [#8334](https://github.com/bobmatnyc/trusty-tools/pull/8334), then pair
   each slot with a fixed-path tree,
   [#8343](https://github.com/bobmatnyc/trusty-tools/issues/8343).**
   Unmeasured.
5. **[Language: Rust] Dev-profile `debug = "line-tables-only"`.** This
   invalidates every warm target directory once, so it lands when no long
   build is running. Unmeasured.
6. **[Project] Consolidate trusty-mpm integration tests** only after the
   baseline shows the link-time share, keeping the two `$HOME`-mutating
   files isolated. Unmeasured.
7. **[Language: Rust] Evaluate `cargo-hakari` and `cargo nextest archive`
   last.** Unmeasured.

No speed-up is claimed for any item; item 1 is the measure for the rest.

## Relationship to existing guidance

The [worktree discipline reference](../reference/worktree-discipline.md) governs
creation and delivery. The [Git workflow skill](../../.agents/skills/git-workflow/SKILL.md)
describes manager-owned cleanup. DOC-66's disk section is explicitly draft;
its strategy ordering should not be presented as benchmarked policy.

Existing documents describe both per-repository shared target directories and
isolated concurrent builds. The dynamic-slot implementation should reconcile
that guidance so agents receive one explicit target assignment and job budget,
rather than choosing between conflicting defaults.

This document does not certify that the pool, operation-level admission, or
ownership handoff described above is installed. It provides lifecycle criteria
and experiments for assessing that work.

PM session addition, 2026-09-21 afternoon:
[#8341](https://github.com/bobmatnyc/trusty-tools/issues/8341) tracks the
test-only dependency edges and the `cargo metadata` guard described in the
follow-up findings above.
[#8335](https://github.com/bobmatnyc/trusty-tools/issues/8335) tracks a
supervisor poller wedge found the same day; it is unrelated to builds and
is listed here only so the day's machine state is complete. The bundled
`rust-build-performance` skill carries a new dev-dependency rule matching
finding 5 above. ADR-0036's statement that the harness exposes no way to
relocate worktrees predates the `WorktreeCreate` hook and needs a
follow-up note.

## Primary external references

- [Git worktree documentation](https://git-scm.com/docs/git-worktree): creation,
  shared versus per-worktree state, removal, locking, and metadata pruning.
- [Cargo build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html):
  build-directory configuration and compiler-cache integration.
- [Cargo build timings](https://doc.rust-lang.org/cargo/reference/timings.html):
  measuring compilation and dependency concurrency.

Added by the PM session, 2026-09-21 afternoon:

- [Claude Code hooks](https://code.claude.com/docs/en/hooks): the
  `WorktreeCreate`/`WorktreeRemove` hook pair referenced in finding 8.
- [Claude Code worktrees — customize worktree creation](https://code.claude.com/docs/en/worktrees#customize-worktree-creation):
  the `WorktreeCreate` hook's contract with `--worktree`, `isolation:
  "worktree"`, and background sessions.
- [mozilla/sccache#2521](https://github.com/mozilla/sccache/issues/2521):
  `SCCACHE_BASEDIRS`, added in sccache 0.14.
- [mozilla/sccache#2863](https://github.com/mozilla/sccache/issues/2863):
  wrong-code risk from base directories plus preprocessor cache mode on
  sccache 0.18.0 and later.
- [rust-lang/cargo#14136](https://github.com/rust-lang/cargo/issues/14136):
  `-Zchecksum-freshness`, nightly-only at MSRV 1.94.

Git worktree and Cargo build-cache documentation were consulted during the
September 21 investigation. Local observations above remain historical snapshots.

## 1.6.5 release-plan review

The owner authorized updating the existing issues, including by comment, after
review of [milestone 88](https://github.com/bobmatnyc/trusty-tools/milestone/88).
These are acceptance clarifications, not claims that implementation has shipped.

- **Release scope:** distinguish core build-efficiency gates from accompanying
  work without moving or closing issues. Compiler-only admission (#8297), Rust
  output slots (#8261), source-worktree lifecycle (#8343), and dependency-graph
  work (#8341) need installed-runtime evidence and comparable before/after
  measurements. Their directly required recovery and cleanup work remains part
  of acceptance.
- **Separate lifetimes:** a task owns its source tree across sequential agent
  handoffs; actual build operations consume compilation capacity. Output remains
  exclusively owned through tests or other artifact consumers. Cancellation,
  restart, and reassignment tests must prove that an old completion cannot
  release a newer lease at the same path.
- **One degraded-admission contract:** retain #8261's bounded fixed-ceiling
  fallback when a capacity reading is unavailable, visibly marked degraded or
  unknown. Apply that same rule in #8329 instead of its contradictory blanket
  load-read refusal. Valid exclusive leases and verifiable target isolation
  remain mandatory; an unreadable ownership state is not permission to build.
  Retain the owner-approved load factor and memory floor. Define the memory
  input using the host metrics reader's available-memory value, not raw unused
  RAM, with explicit units and sample freshness.
- **Safe fixed-path reuse:** #8343 must preserve valuable dirty, untracked,
  ignored, detached, and unpushed work; verify writers and consumers have exited;
  and prove the new revision produces its own artifacts. `git switch` alone is
  neither a cleanup policy nor proof of safe reuse. The native-hook experiment
  remains a prerequisite to the mechanism choice.
- **Dependency-graph evidence:** #8341's September 21 audit found eight added
  crates for trusty-mpm's trusty-review edge, versus roughly 135–198 for other
  audited edges. Counts do not establish time saved. Explicitly execute relocated
  cross-crate tests in CI, retain their coverage, and evaluate the resolved graph
  with a reasoned exception mechanism rather than banning useful integration
  tests. Use the controlled experiments above to establish actual gains.

Track these within the existing issues; no additional issue, implementation
claim, or blanket concurrency increase follows from this review.
