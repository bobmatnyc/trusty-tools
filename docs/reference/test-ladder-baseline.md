# Rust Test Ladder — Baseline-Failure Protocol

> **The ladder table, the rung selection rule, and "never make a red gate
> green by deleting coverage" live in
> [`.trusty-mpm/INSTRUCTIONS.md`](../../.trusty-mpm/INSTRUCTIONS.md) /
> `CLAUDE.md`** (Rust Test Ladder). This page is the Rust-specific detail for
> telling a pre-existing red from one your branch caused — consult it while
> triaging a failing gate, not to decide which rung to run.

The six-step baseline-failure protocol (establish whose red it is, fix if
branch-caused, append to the canonical issue if tracked, one canonical issue if
not, and the literal report string) lives in `tm-pr-workflow`. It is **not**
restated here. What follows is only what that generic version cannot carry.

## Known-Environmental On This Machine — Expect These, Do Not Re-File Them

| Gate | Where | Why it fails independent of your branch |
|---|---|---|
| The `trusty-search` filesystem-watcher tests (~6; the set drifts) | `crates/trusty-search/src/service/watch_loop.rs`, `watcher.rs`, `watcher_manager.rs` | FSEvents delivery on this host is nondeterministic. They fail on any branch, and they still fail under `-- --test-threads=1` — so it is the machine, not parallel-test load |
| `execute_doctor_against_test_daemon` | `crates/trusty-mpm/src/client/executor/tests.rs` | It takes 9–13 s against a 10 s client timeout, so it loses on timing alone under any load |

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
