# Rust Test Ladder — Gate Commands and Baseline-Failure Protocol

> **Which rung a change lands on — the change classes, the risk labels, and
> "never make a red gate green by deleting coverage" — is decided in
> [`CLAUDE.md`](../../CLAUDE.md)** (Rust Test Ladder). Choose the rung there,
> then come here for the exact command to run and for triaging a red gate.

The baseline-failure protocol (establish whose red it is, fix if branch-caused,
hand a pre-existing red to `tm-ticketing` for its disposition, and the literal
report string) lives in `tm-workflow`. It is **not** restated here. What follows is only what that generic version cannot carry.

## Per-Rung Gate Commands

Run the smallest deterministic gate that covers the change's blast radius. Paste
the command you actually ran into the PR body so a reviewer can see which rung
was exercised.

| # | Development proof | PR gate — the command to run | Hardening / release gate |
|---|---|---|---|
| 1 | Read the rendered file | `bash scripts/check_sld.sh` (plus `check_doc_numbers.sh` / `check_line_cap.sh` if those surfaces were touched). No Cargo test by default. | CI required checks only |
| 2 | Fail-before / pass-after, repeated: `cargo test -p <crate> <test> -- --exact --nocapture` run ~10× | `cargo fmt --check` && `cargo test -p <crate> --no-fail-fast`; add `-- --test-threads=1` when the flake is isolation-shaped | `cargo test --workspace` **only** when shared test infrastructure changed |
| 3 | One targeted regression test that provably fails before the change | `cargo fmt --check` && `cargo check -p <crate>` && `cargo clippy -p <crate> -- -D warnings` && `cargo test -p <crate> --no-fail-fast` | Workspace gate only when release policy requires it |
| 4 | Targeted regression plus `cargo test -p <lib>` | rung 3 for the library, then `cargo check --workspace` && `cargo test -p <consumer> --no-fail-fast` for **each direct dependent** | `cargo test --workspace` && `cargo clippy --workspace --all-targets -- -D warnings` at HARDEN/release |
| 5 | Targeted plus failure-path and concurrency tests | rung 4, plus `cargo test -p <crate> --no-fail-fast -- --include-ignored` for gated integration coverage, plus an adversarial review round (`code-critic`) | full workspace, `cargo audit`, and for release tooling `scripts/check-publish-ready.sh <crate>` && `scripts/preflight-publish.sh <crate>` |
| 6 | Rust crate tests **plus** direct UI/API evidence (curl the route, call the MCP tool, load the page) | rung 3 or 4 for the Rust side, plus `pnpm -C crates/<crate>/ui test` (where the package defines one; otherwise `… build`) and one smoke run of the binary | full product/e2e gate plus `cargo test -- --include-ignored` when hardening |

Why `cargo test --workspace` is absent from rungs 1–3, and the "scope down,
never scope away" line that constrains picking a lower rung, are stated with the
ladder itself in [`CLAUDE.md`](../../CLAUDE.md) — not repeated here.

### Every rung carries `--no-fail-fast`, and the reason is not politeness

Cargo runs each test target as its own binary and, by default, stops issuing
further targets as soon as one target reports a failing test. It does not run
every target and report the aggregate. A single failing `--lib` test therefore
zeroes out every integration target behind it, and the invocation exits having
covered far less than its counts suggest.

Worked example, on a two-target project whose `--lib` target fails:

```
$ cargo test                      # only the --lib target ran
test result: FAILED. 0 passed; 1 failed; 0 ignored
error: test failed, to rerun pass `--lib`

$ cargo test --no-fail-fast       # the integration target ran as well
test result: FAILED. 0 passed; 1 failed; 0 ignored
error: test failed, to rerun pass `--lib`
test result: ok. 1 passed; 0 failed; 0 ignored
```

This is not hypothetical here. The #5324 implementer found
`--test tm_hook_pm_guard` — the regression test that fix depends on — had never
executed once across a full session of `cargo test -p trusty-mpm` runs, because
the `--lib` target kept failing first; `--no-fail-fast` ran all 46 targets
green. It recurred on PR #5904, where the `execute_doctor_against_test_daemon`
timing flake masked 49 other targets. `bench_stale_assets_for_many` and the
`tm_hook_pm_guard` family (#5914) supply the same trigger.

It also changes what a pasted count proves. `5280 passed; 0 failed` from a run
WITHOUT the flag is evidence about the targets that ran, not about the crate —
so name the flag beside the counts, or a reviewer cannot tell the two apart.

### `#[serial]` does not serialize under nextest

CI's `test-shard` job runs `cargo nextest run --workspace`, and nextest gives
every test its own process. `serial_test`'s `#[serial]` is an in-process lock,
so it serializes nothing there. Measured 2026-08-27: two `#[serial]` tests
sharing a key interleaved across two pids under nextest
(`one ENTER` / `two ENTER` / `two EXIT` / `one EXIT`) and ran strictly one after
the other in a single pid under `cargo test`.

The `$HOME` isolation PR #4120 established is not the casualty #4162 predicted.
A `set_var("HOME")` is process-local, so process-per-test isolation is strictly
STRONGER than the in-process lock for env state — measured, the sibling test
read the real `$HOME` while the mutating test held its own redirect, and it
cannot observe the redirect at all. Running the `$HOME`-touching targets
(`session_manager_mvp`, `local_spawn`) under both runners added no entry to the
real `~/.claude.json`: 2828 projects before and after, both times.

What DOES lapse is any `#[serial]` guarding a resource shared ACROSS processes —
a fixed path, a fixed port, one real file. Guard those with
`#[serial_test::file_serial]`, which takes a filesystem lock and needs
serial_test's `file_locks` feature; `#[serial]` and `#[file_serial]` lock
independently, so a resource must pick one and use it everywhere. And keep
redirecting `$HOME` per test: the redirect is what keeps a test out of the
operator's real config, and always was — `#[serial]` never did that job.

### `-p <crate>` is only a gate when the crate's default features compile the code

Every `-p <crate>` cell above assumes the crate's default feature set compiles
what you changed. `trusty-common` declares `default = []` and gates 25+ modules,
so `cargo test -p trusty-common` used to run 328 of the crate's ~2062 tests and
exit 0 — a green that covered neither `memory_core` nor `uds` nor `sld`, and
held even when a `memory_core` file did not compile (#4901; PR #4899 shipped on
that green). Since #4901 that form is a `compile_error!` instead. Substitute
these wherever a rung says `cargo test -p trusty-common`:

| What you changed | Command |
|---|---|
| `memory_core` | `cargo test -p trusty-common --features memory-core,embedder-test-support` |
| any other gated module | `cargo test -p trusty-common --features <feature>` |
| only unconditional modules | `cargo test -p trusty-common --features unconditional-only` |

The same shape exists wherever a non-default feature gates real code —
`trusty-common`'s `codex-config` is enabled by no crate in the workspace, so even
`cargo clippy --workspace` never compiles it and CI lints it in a dedicated step
(#5264). Before reading a crate-scoped green as a gate, check whether the module
you edited is behind a `#[cfg(feature = …)]` the command did not enable.

## The Three Stages in Cargo Terms

`Skill(skill="tm-workflow")` ("Test Scope Widens by Stage") sets the framework
rule for every project: developing runs the tests covering the new or changed
code only, merging runs the full test files that changed, publishing runs the
full corpus. This is what each stage is in this workspace.

| Stage | Cargo scope | Command |
|---|---|---|
| Developing | the changed code's own tests | `cargo test -p <crate> <test-or-module> -- --exact --nocapture` |
| Merging | every crate whose test files the diff changed | `cargo test -p <crate>` for each, alongside the `fmt`/`check`/`clippy` gates the rung calls for |
| Publishing | the workspace | `cargo test --workspace` && `cargo clippy --workspace --all-targets -- -D warnings`, plus `cargo audit` and the publish preflight scripts |

**Why merging is `-p <crate>` and not a per-file run.** Cargo has no "run this
file" unit. A `#[cfg(test)] mod tests` compiles into its crate's test binary, and
an integration test under `crates/<crate>/tests/` is its own binary but is still
only addressable through its crate. `cargo test -p <crate>` is the smallest run
that contains every test file the diff touched, so for this workspace it **is**
the merge scope rather than an over-run of it.

## How the Three Stages Compose With the Six Rungs

Two axes, both binding. The rung says how much rigour the change earns — which
gates run at all, whether ignored tests count, whether a `code-critic` round is
owed. The stage says how wide each of those gates runs. Pick the rung from
[`CLAUDE.md`](../../CLAUDE.md), then read the stage off where you are in the
delivery chain.

The per-rung table above already encodes the stages column by column:

- **Development proof** is the developing stage.
- **PR gate** is the merging stage.
- **Hardening / release gate** is the publishing stage.

**A rung-3 change: `cargo test -p <crate>` is the MERGE scope, not the develop
scope.** While developing, run only the targeted regression test that provably
fails before the change — `cargo test -p <crate> <test> -- --exact`. Before
merging, run the crate in full: `cargo fmt --check` && `cargo check -p <crate>`
&& `cargo clippy -p <crate> --all-targets -- -D warnings` && `cargo test -p
<crate>`. A rung-3 change owes the workspace nothing at either stage.

**Rung 4's `cargo check --workspace` stays at merge.** It is a compile gate, not
a test run, so "full corpus only when publishing" does not reach it. Its
companion `cargo test -p <consumer>` for each direct dependent is also merge
scope, for the reason the framework rule gives: changing a shared library's
public API changes what every dependent's test files mean, which makes those
files changed even though the diff never opened them. Rung 4's `cargo test
--workspace` sits in the hardening column precisely because it is the publish
stage.

**Rung 5's `--include-ignored` stays at merge too.** It is crate-scoped, and an
`#[ignore]`d test living in a test file your diff changed is part of that file —
the merge stage runs changed files *in full*, which is exactly what
`--include-ignored` does for a crate. What rung 5 defers to publishing is the
workspace run, `cargo audit`, and `scripts/check-publish-ready.sh` /
`scripts/preflight-publish.sh`.

**Rung 2's shared-test-infrastructure caveat is the one place merge scope can
reach the workspace.** Same logic as rung 4's dependents: a change to a shared
fixture or test harness changes what every test file using it means. When that
happens the workspace run is a merge obligation, not a deferral to publish — the
table's "only when shared test infrastructure changed" is the trigger.

**`cargo test --workspace` is keyed to the stage, not the rung.** `CLAUDE.md`
places it at the publish boundary; rungs 4–6 are named there only because they
are the rungs that reach that boundary. A rung-4 PR does not owe a workspace test
run to merge.

**The pipeline follows from this.** The CI `Rust tests (pre-publish gate)` job is
a pre-build / pre-publish gate, not a pre-merge one, and it is not among `main`'s
required status contexts — the authoritative, current list is always `gh api
repos/bobmatnyc/trusty-tools/branches/main/protection --jq
'.required_status_checks.contexts'`, not a copy enumerated here. It no longer runs
on pull requests at all: it runs on every push to `main`, and on demand via
`workflow_dispatch` (Actions → CI → "Run workflow"). `preflight-publish.sh` CHECK 1
refuses to publish any commit that is not `origin/main`, so the tree that gets
published is always a tree the shards have run against.

**A failing check still blocks at every stage.** Deferring *when* the workspace
suite runs changes nothing about what a red one means. On `main` a failure opens
or updates the `ci-red-main` tracking issue via `ci.yml`'s `notify-main-failure`
job and fails the run. A sibling workflow's failure does the same through
`.github/workflows/red-main-notify.yml`, which watches every other push-to-main
workflow over `workflow_run` — `needs:` cannot cross workflow files (#5657).

🔴 **Scoping down by stage is a claim you must be able to prove**, exactly as
scoping down by rung is. It is never licence to make a red gate green by
deleting, `#[ignore]`-ing, `cfg`-gating, `--exclude`-ing, or `--lib`-narrowing
coverage. A red gate blocks at every stage; only a *pending* full-corpus check is
tolerated at merge, never a failing one.

## How Much Gate Output the PR Body Owes

A PR body may summarise a **passing** gate as command + counts + scope —
`cargo test -p trusty-mpm — 214 passed, 0 failed` — because the reviewer's
question there is which rung ran, not what scrolled past.

Raw output stays **mandatory** for failures, flakes, performance claims, and
disputed results. Agent-to-PM reporting keeps raw output in all cases
(`BASE-AGENT.md`: never summarise test results in your own words).

## Known-Environmental On This Machine — Expect These, Do Not Re-File Them

| Gate | Where | Why it fails independent of your branch |
|---|---|---|
| The `trusty-search` filesystem-watcher tests (~6; the set drifts) | `crates/trusty-search/src/service/watch_loop.rs`, `watcher.rs`, `watcher_manager.rs` | **Retired as a known-environmental row by #4731** — the six tests no longer carry the timing assumption that produced it. They wait on their condition and re-apply the save once per debounce window (`service::watch_test_support`), so a save lost to an FSEvents queue overflow is retried rather than stranding a fixed 2–3 s deadline. A red run here is now a real signal: file it. One correction to what this row used to say: the earlier candidate mechanism — a save landing *before* OS watch registration finishes — does not apply, because `notify` 6.1.1's FSEvents backend does not return from `watch()` until after `FSEventStreamStart`. The actual loss path is an FSEvents queue overflow: it sets `MustScanSubDirs` on an event whose path is a DIRECTORY to rescan rather than the file that changed, `notify` attaches that directory path (`fsevent.rs` `callback_impl` calls `add_path` on every translated event, Rescan included), `notify-debouncer-mini`'s `DebouncedEvent` keeps only path and kind so `Flag::Rescan` is discarded, and the watcher sees an ordinary modify of a directory — dropped at the consumer's `is_dir()` guard. The specific save is never learned |
| `create_index_cannot_register_while_a_delete_is_tearing_the_id_down` | `crates/trusty-search/src/service/server/tests_3049.rs:522` | Races an async index-DELETE teardown against a subsequent CREATE. Measured on `origin/main` at commit `f87f55377`: 5 pass / 1 fail over 6 runs. Failure assertion: `once teardown is done the recreate is an ordinary create and must succeed`, `left: 500`, `right: 200`. CI reproduced both outcomes on identical head SHA `1e62da13d` (run `31577687366` failed, run `31577755214` succeeded). The `500` status is the create hitting a conflict because teardown has not yet released the index id; `200` is expected once it has |
| `execute_doctor_against_test_daemon` | `crates/trusty-mpm/src/client/executor/tests.rs` | It takes 9–13 s against a 10 s client timeout, so it loses on timing alone under any load |
| `stale_assets_for_many_reads_shared_agent_dir_once_for_the_whole_fleet` | `crates/trusty-mpm/src/` (test-only) | Parallel-run race on `$HOME` mutation between tests. Fails 3 of 5 runs on a clean baseline worktree with no branch changes. Passes under `-- --test-threads=1` |
| `safe_session_cwd_replaces_home_and_missing` | `crates/trusty-mpm/src/` (test-only) | Same `$HOME`-mutation race as `stale_assets_for_many_reads_shared_agent_dir_once_for_the_whole_fleet`, same evidence run (5 runs: 3 failed, 2 passed), same remedy (`-- --test-threads=1`) |
| `meta_run_demo_writes_and_verifies_artifact` | `crates/trusty-mpm/src/` (`--include-ignored` only) | Times out at ~187s (`launch_outcome: "timed-out"`). It launches a real Claude Code session, so it depends on external process availability. Reproduced identically on a clean baseline worktree with branch changes stashed |
| `bench_stale_assets_for_many` | `crates/trusty-mpm/src/` (`--include-ignored` only) | Load-sensitive benchmark: fails under a loaded run, passes in isolation and on a clean `--lib --include-ignored` run (4579 passed, 0 failed) |
| `ensure_managed_config_dir_emits_the_frozen_skill_warning` | `crates/trusty-mpm/src/` (test-only) | Same `#[serial]`-guard family as `stale_assets_for_many_reads_shared_agent_dir_once_for_the_whole_fleet` above — `#[serial]` only serializes against other `#[serial]` tests, so a parallel non-serial test's globally-installed tracing subscriber can still beat this one's capturing subscriber. Tracked at #4931 |
| `compose_session_instructions_display_matches_live_prompt` | `crates/trusty-mpm/src/` (test-only) | Asserts by diffing two enumerations of the live `~/.claude/agents` directory instead of a fixture; concurrent agent deployment between the two calls changes the diff (observed: 5 user-tier agents — `copyeditor`, `pangram-editor`, `proofreader`, `writer`, `writing-critic`). Passes in isolation. Same race as `compose_session_instructions_display_matches_live_prompt_with_override` below. Tracked at #4937 |
| `compose_session_instructions_display_matches_live_prompt_with_override` | `crates/trusty-mpm/src/` (test-only) | Same live-`~/.claude/agents` race as `compose_session_instructions_display_matches_live_prompt` above. Passes in isolation. Tracked at #4937 |
| The `stale_assets_for_many_*` test family (`daemon::managed_routes::tests`) | `crates/trusty-mpm/src/` (test-only) | Load-sensitive: 2 of 3 full-suite runs failed on a pristine `origin/main` worktree with no local changes (measured 2026-08-06). Mechanism documented in the tests' own doc comments — a process-global deployed-read log (see #4619 review note). No canonical tracking issue exists for this one |

**Parallel-run failures in `trusty-mpm`:** A failure that appears only in
parallel test runs is not evidence your branch broke something. Re-run with
`-- --test-threads=1` to isolate the machinery from parallel interference; if
it goes green, the failure was isolation-specific, not a correctness defect.
Isolation races in this crate are pre-existing and documented above.

🔴 **The correct response is to prove the failure is pre-existing — never to
`#[ignore]`, `cfg`-gate, or `--exclude` a failing test.** If the failing crate
depends on nothing you changed, an empty path list proves it. Otherwise —
across a dependency edge such as `trusty-search` on `trusty-common` — that
path list can come back empty while the branch is still guilty, and the valid
proof is reproducing the failure on `origin/main` (step 2 below; the
shared-crate caveat is at step 5). When the path list is the applicable proof,
paste it in the PR:

<!-- Load-bearing order: keep both branches above this block — moving the
     block ahead of them re-opens the #5594 misreading. -->
```bash
git diff --name-only origin/main...HEAD -- crates/trusty-search/ \
                                           crates/trusty-mpm/src/client/
```

`create_index_cannot_register_while_a_delete_is_tearing_the_id_down` above is a
worked example of that `origin/main` reproduction, measured directly on
`origin/main` (documented by
[#5607](https://github.com/bobmatnyc/trusty-tools/pull/5607)).

## Telling A Pre-Existing Red From One You Caused — Crate-Scoped Confirmation

1. **Re-run the failure alone on your branch**, not the whole suite:
   `cargo test -p <crate> <test> -- --exact --nocapture`. A single named test
   removes ordering and load from the picture.
2. **Run the identical command against `origin/main` in a second worktree** —
   never by mutating the main checkout, and never with `git stash`:
   ```bash
   git worktree add .claude/worktrees/baseline-<n> origin/main
   cd .claude/worktrees/baseline-<n> && cargo test -p <crate> <test> -- --exact
   ```
   Failing on both is the evidence that the branch did not cause it.
3. **Serialize before blaming isolation:** `-- --test-threads=1`. If it still
   fails serialized, parallel interference is not the explanation and "flaky
   under load" is the wrong diagnosis.
4. **Check the path evidence:** if the failing crate depends on nothing you
   changed, an empty `git diff --name-only origin/main...HEAD` for it means the
   red is not yours; otherwise an empty diff proves nothing — see the
   shared-crate caveat next.
5. 🔴 **The shared-crate caveat:** a green `cargo test -p <your-crate>` does
   **not** clear you when you changed `trusty-common`, `trusty-embedderd`, or any
   other shared library. A red in a *dependent* crate is yours until rung 4 of
   the ladder says otherwise. This is the one case where scoping down is
   the wrong instinct.

Report it in the shape `tm-workflow` mandates, naming the crate-scoped gate:
`change-specific gates pass; cargo test -p trusty-search blocked by canonical
issue #N`. Never "all tests pass" while a gate is red, whoever caused it.
