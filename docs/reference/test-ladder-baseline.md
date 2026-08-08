# Rust Test Ladder — Gate Commands and Baseline-Failure Protocol

> **Which rung a change lands on — the change classes, the risk labels, and
> "never make a red gate green by deleting coverage" — is decided in
> [`CLAUDE.md`](../../CLAUDE.md)** (Rust Test Ladder). Choose the rung there,
> then come here for the exact command to run and for triaging a red gate.

The six-step baseline-failure protocol (establish whose red it is, fix if
branch-caused, append to the canonical issue if tracked, one canonical issue if
not, and the literal report string) lives in `tm-pr-workflow`. It is **not**
restated here. What follows is only what that generic version cannot carry.

## Per-Rung Gate Commands

Run the smallest deterministic gate that covers the change's blast radius. Paste
the command you actually ran into the PR body so a reviewer can see which rung
was exercised.

| # | Development proof | PR gate — the command to run | Hardening / release gate |
|---|---|---|---|
| 1 | Read the rendered file | `bash scripts/check_sld.sh` (plus `check_doc_numbers.sh` / `check_line_cap.sh` if those surfaces were touched). No Cargo test by default. | CI required checks only |
| 2 | Fail-before / pass-after, repeated: `cargo test -p <crate> <test> -- --exact --nocapture` run ~10× | `cargo fmt --check` && `cargo test -p <crate>`; add `-- --test-threads=1` when the flake is isolation-shaped | `cargo test --workspace` **only** when shared test infrastructure changed |
| 3 | One targeted regression test that provably fails before the change | `cargo fmt --check` && `cargo check -p <crate>` && `cargo clippy -p <crate> -- -D warnings` && `cargo test -p <crate>` | Workspace gate only when release policy requires it |
| 4 | Targeted regression plus `cargo test -p <lib>` | rung 3 for the library, then `cargo check --workspace` && `cargo test -p <consumer>` for **each direct dependent** | `cargo test --workspace` && `cargo clippy --workspace --all-targets -- -D warnings` at HARDEN/release |
| 5 | Targeted plus failure-path and concurrency tests | rung 4, plus `cargo test -p <crate> -- --include-ignored` for gated integration coverage, plus an adversarial review round (`code-critic`) | full workspace, `cargo audit`, and for release tooling `scripts/check-publish-ready.sh <crate>` && `scripts/preflight-publish.sh <crate>` |
| 6 | Rust crate tests **plus** direct UI/API evidence (curl the route, call the MCP tool, load the page) | rung 3 or 4 for the Rust side, plus `pnpm -C crates/<crate>/ui test` (where the package defines one; otherwise `… build`) and one smoke run of the binary | full product/e2e gate plus `cargo test -- --include-ignored` when hardening |

Why `cargo test --workspace` is absent from rungs 1–3, and the "scope down,
never scope away" line that constrains picking a lower rung, are stated with the
ladder itself in [`CLAUDE.md`](../../CLAUDE.md) — not repeated here.

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
| The `trusty-search` filesystem-watcher tests (~6; the set drifts) | `crates/trusty-search/src/service/watch_loop.rs`, `watcher.rs`, `watcher_manager.rs` | FSEvents delivery on this host is nondeterministic. They fail on any branch, and they still fail under `-- --test-threads=1` — so it is the machine, not parallel-test load |
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

🔴 **The correct response is to prove your diff touches zero files in those
crates — never to `#[ignore]`, `cfg`-gate, or `--exclude` them.** The proof is a
path list, and empty output is the evidence; paste it in the PR:

```bash
git diff --name-only origin/main...HEAD -- crates/trusty-search/ \
                                           crates/trusty-mpm/src/client/
```

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
4. **Check the path evidence:** `git diff --name-only origin/main...HEAD`. If the
   failing crate does not appear there, and you did not touch a shared library it
   depends on, the red is not yours.
5. 🔴 **The shared-crate caveat:** a green `cargo test -p <your-crate>` does
   **not** clear you when you changed `trusty-common`, `trusty-embedderd`, or any
   other shared library. A red in a *dependent* crate is yours until rung 4 of
   the ladder says otherwise. This is the one case where scoping down is
   the wrong instinct.

Report it in the shape `tm-pr-workflow` mandates, naming the crate-scoped gate:
`change-specific gates pass; cargo test -p trusty-search blocked by canonical
issue #N`. Never "all tests pass" while a gate is red, whoever caused it.
