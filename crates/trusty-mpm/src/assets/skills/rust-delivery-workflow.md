---
name: rust-delivery-workflow
description: "Rust delivery process under agent dispatch: commit before the gate chain, match the local toolchain to CI's clippy pin, cap concurrent cargo builds, keep gate output to a verdict, and batch installs before live verification. Use when planning or running the gates for a Rust change, not when a build feels slow."
user-invocable: false
version: "1.0.0"
category: agent-reference
effort: low
---
# Rust Delivery Workflow

Process rules for shipping a Rust change through an agent-dispatched pipeline.
Everything here is about *when and how* to run a gate, never about what a
cargo command spells or how to make a build faster.

## 1. Scope and Precedence

Three skills divide the Rust surface. Keep them apart.

| Skill | Owns |
|---|---|
| `rust-delivery-workflow` (this one) | Delivery process: gate ordering, crash survival, toolchain parity, build concurrency, batching |
| `rust-build-performance` | Inner-loop speed: `cargo check` first, `--timings`, dependency and feature trimming, incremental cache, sccache mechanics |
| `cargo-commands` (where a project ships one) | Command spelling: release builds, feature-gated runs, single tests by name |

This skill points at the other two rather than restating them. A project's own
`CLAUDE.md` and `docs/` outrank all three, because only the project knows its
crate list, its CI pin, and its test ladder. When this skill and the project
disagree, the project wins.

## 2. Commit Before the Gate Chain

Commit and push the branch as soon as the files exist, before starting a gate
chain that runs for minutes. Then add follow-up commits instead of amending.

A gate chain is the longest uninterruptible stretch of a Rust task. A crash, a
context exhaustion, or a tool timeout during it destroys every uncommitted
edit, and the work has to be redone from scratch. A commit costs seconds and
makes the loss recoverable.

This is separate from the WIP-commit rule that guards a destructive `git
checkout` during verification. That one protects against a command you run;
this one protects against the process dying.

## 3. CI-Equivalent Clippy

Verify that your local toolchain matches the project's CI pin before you
trust a local clippy pass. The CI workflow file is the source of truth for
that pin, not your memory and not the toolchain your shell happens to
resolve.

Two failures this prevents:

- **Toolchain drift.** Clippy adds lints between releases. A local run on an
  older toolchain reports zero warnings on code that CI's newer clippy denies.
- **Scope drift.** CI lints the whole workspace with an explicit exclude list.
  A crate-scoped local `cargo clippy -p <crate>` exit 0 is not evidence that
  the CI job passes, because it never compiled the other crates.

Read the pin out of the workflow file, run clippy through that exact
toolchain, and print the clippy version so the run is self-identifying. In
this workspace the recipe lives in `docs/reference/ci-gates.md` under
"Running CI's clippy locally"; the exact invocation belongs there and not
here, because the exclude list and the pin are project facts.

Incident: PR #5488, where a local clippy pass preceded a red CI clippy job.

## 4. Build Concurrency

A Rust build is CPU and RAM bound. Two concurrent `cargo build` or `cargo
test` invocations on one host can thrash swap or get OOM-killed, and worktree
isolation does not help. Isolation gives each agent its own `target/`
directory, which removes cargo's build-lock contention (#7895); it removes no
memory pressure at all, because both builds still run on the same machine.

The symptom is an exit code of 137 with no test failure printed, which reads
like a bug in the code under test.

Two mechanisms enforce the cap, and neither is a number you should hardcode:

- `tm` caps concurrent build-capable dispatches (#8193).
- The doctor check reports the machine's own build environment and the job
  count it supports (#6868).

Take the number from the machine, not from this file. Then prefix it inline on
every cargo invocation:

```bash
CARGO_BUILD_JOBS=<n> cargo test -p <crate> --no-fail-fast
```

Inline on every command, because an agent's shell environment does not persist
between tool calls. An `export` in one call is gone by the next, so a single
`export CARGO_BUILD_JOBS=2` at the top of a task silently protects nothing.

Pointing every worktree at one shared `CARGO_TARGET_DIR` per repo is the knob
that actually pays: a fresh worktree's cold full-workspace build took ~200 s on
the 16-core reference host, 103 s against a target directory another worktree
had warmed, and 17 s from that path again. `tm doctor`'s `rust_build_env` row
prints the whole prefix — `CARGO_TARGET_DIR=<dir> CARGO_BUILD_JOBS=<n>
[RUSTC_WRAPPER=sccache] SKIP_UI_BUILD=1` — so take it from there rather than
assembling it by hand.

## 5. Gate-Output Economy

A gate is a declarative process. It owes you a verdict, not a play-by-play.
Redirect the chain into a scratch file, check `EXIT=$?`, and read the file
only when the exit code is non-zero.

The rule and its mechanics already live in BASE-AGENT's "Verification
Hygiene" and "Never end a gate chain in a pipe" sections. Read those; this
skill only notes that a Rust gate chain is where the cost lands hardest,
because `cargo test` output is long and mostly `ok`.

Two Rust-specific corollaries:

- Never re-run a gate to check on its progress. A second `cargo test` blocks
  on the first one's lock or duplicates the work, and either way it buys no
  information the first run will not print.
- Never end a chain in a pipe. `cargo test ... | tail` exits 0 on a failing
  suite, because a pipeline reports the last command's status.

Reference: PR #4790, which landed the gate-economy prose.

## 6. sccache Posture

`rust-build-performance` section 6 covers what sccache caches, why a
workspace path crate gets no cross-worktree hit under incremental
compilation, and where the win actually comes from. Read it there.

The process rule is the one this skill adds: recommend sccache to the
operator, never silently enable it. Writing a `rustc-wrapper` into a shared
config changes every build on the machine, including builds you did not run,
so it is an operator decision and not a side effect of an unrelated task.

Measured on this workspace, sccache was NEUTRAL — a cold worktree build took
~200 s with it and without it, because path crates dominate the graph and their
incremental artifacts are not cacheable. `tm doctor`'s `rust_build_env` row
reports whether sccache is on `PATH` and wired as `build.rustc-wrapper`, and
emits `RUSTC_WRAPPER=sccache` in the prefix line only when the operator's
`build.sccache` config asked for it; it never writes `~/.cargo/config.toml`.

## 7. Batch Merges, Then One Install and One Probe

For a change class whose close requires live proof on an installed binary,
merge the batch first, then run one `cargo install` and one live-probe pass
across all of it. Do not install and probe once per issue.

Each install is a full release build. Running one per merged issue multiplies
the most expensive step in the pipeline by the batch size and proves nothing
extra, because every issue in the batch ships in the same binary.

The probe pass still has to name each issue's own observable behaviour
separately. Batching the build never means batching the evidence.

This mirrors the "bugfix batching is the default" norm in `tm-workflow` for
PRs, applied one stage later.

## 8. Test Scope by Stage

`tm-workflow`'s "Test Scope Widens by Stage" table already states the rule:
develop against the changed code, gate a merge on the full changed test files,
and reserve the full corpus for publish. It is generic and already codified.

Read it there. The only Rust-specific note is that the publish rung is the
one that justifies a bare `cargo test --workspace`; nothing earlier does.
