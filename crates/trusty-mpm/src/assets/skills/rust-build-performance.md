---
name: rust-build-performance
description: "Practical Rust build-performance discipline for the inner dev loop: cargo check first, measure with --timings before tuning, trim the dependency/feature graph, preserve incremental compilation, and use sccache across worktrees. Use when a Rust build feels slow or before reaching for compiler-flag tricks."
user-invocable: false
version: "1.0.0"
category: agent-reference
effort: low
---
# Rust Build Performance

The big wins live in the **development loop** and the **dependency graph**,
not in obscure compiler flags. Optimize those two first; treat flags and LTO
tuning as a last resort for measured hotspots, not a starting point.

## 1. Core Principle

Practical inner loop, in order:

1. `cargo check` (or `cargo check -p <crate>`) while writing code — fast,
   catches type/borrow errors, never blocks on codegen.
2. Targeted tests for the crate you're touching — `cargo test -p <crate>`.
3. A full `cargo build` (or `cargo build --workspace`) only occasionally —
   before a PR, before a release, when you need a runnable binary.

Reach for `cargo check` by default; reach for a full build only when you
actually need generated code (running the binary, running tests that need it,
or the workspace-wide quality gate before shipping).

## 2. `cargo check` While Coding

```bash
cargo check                 # whole workspace, no codegen
cargo check -p trusty-search # narrow to the crate you're actually editing
```

`cargo check` runs the same type-checking and borrow-checking as `cargo
build` but skips code generation, so it's dramatically faster for the
edit-check-edit cycle. In a large workspace (this one has 21+ crates), always
narrow with `-p <crate>` unless you specifically need cross-crate diagnostics
— checking the whole workspace on every keystroke-adjacent save wastes the
exact time `cargo check` exists to save.

**This does not change the shipping gate.** This project's quality bar still
requires the full `cargo build --workspace`, `cargo test`, `cargo clippy
--workspace --all-targets -- -D warnings`, and `cargo fmt --check` before any
change lands — see the project `CLAUDE.md` Build and Test Commands section.
`cargo check` is for the inner loop only; it never substitutes for the gate.

## 3. Measure Before Tuning

Never guess at what's slow. Profile first:

```bash
cargo build --timings
# → target/cargo-timings/cargo-timing.html
```

Open the HTML report. It shows, per crate: wall-clock compile time, how much
of that is the crate's own codegen vs. waiting on dependencies, build-script
(`build.rs`) time, and where parallelism is blocked (a long serial chain in
the dependency graph caps how much your core count actually helps). Use this
to find the actual bottleneck — a single slow proc-macro crate or a build
script re-running unnecessarily — before touching anything else. Tuning
without a `--timings` report first is optimizing blind.

Reference: <https://doc.rust-lang.org/cargo/reference/timings.html>

## 4. Reduce Dependency / Feature Load

Dependencies and proc-macros dominate clean-build time far more than your own
code usually does. Audit the graph:

```bash
cargo tree --duplicates       # multiple versions of the same crate = wasted compiles
cargo tree --edges features   # which features are pulled in, and by what
```

- Disable default features you don't need
  (`serde = { version = "1", default-features = false, features = ["derive"] }`).
- Remove heavyweight dependencies that provide marginal value — every crate in
  the graph is compile time, not just runtime weight.
- Consolidate duplicate versions of the same crate (`cargo tree --duplicates`)
  — cargo compiles each distinct version separately.
- Don't reflexively enable every feature/workspace member during ordinary
  dev; use `-p <crate>` and feature flags to compile only what you're
  actually exercising.
- Cargo's workspace feature resolver (resolver = "2", already the default for
  this workspace) unifies features per-target rather than globally — verify
  your `Cargo.toml` isn't accidentally forcing broader unification.

**Workspace-specific note:** `[workspace.dependencies]` sharing is already
this repo's convention (see project `CLAUDE.md`) — never pin a dependency
locally if it's already in the workspace table; a locally-pinned duplicate
defeats both dependency-graph hygiene and cargo's version unification.

Reference: <https://doc.rust-lang.org/cargo/reference/features.html>,
<https://doc.rust-lang.org/cargo/commands/cargo-tree.html>

## 5. Preserve Incremental Compilation

The `dev` profile is incremental by default — cargo caches per-crate
compilation artifacts in `target/` and reuses them across builds. Protect
that cache:

- Don't delete `target/` mid-task. A deleted `target/` forces a full cold
  rebuild of the entire dependency graph.
- Don't churn `RUSTFLAGS` between builds — a changed flag set invalidates the
  incremental cache for everything built under it.
- Don't switch feature sets back and forth on the same crate in the same
  session — each switch is effectively a different build configuration.
- Don't switch toolchains mid-task (e.g. stable ↔ nightly, or MSRV bump)
  without expecting a full rebuild.

**Optional faster local dev profile** — for a snappier inner loop, some
engineers add a lighter debug profile locally:

```toml
[profile.dev]
opt-level = 0
debug = "line-tables-only"
incremental = true
```

Treat this as a **local, uncommitted** override
(`~/.cargo/config.toml` or an untracked `Cargo.toml` edit) — do **not** commit
profile changes to the workspace root `Cargo.toml` without explicit team
approval; it affects every contributor's build and debugging experience.

**Worktree-lifecycle note:** this project's parallel-worktree discipline
means every fresh `git worktree add` is a cold build — there is no shared
`target/` to inherit. Don't delete a *live* worktree's `target/` mid-task
expecting a quick rebuild; merged-PR worktrees have their `target/` reclaimed
separately by hygiene policy (#2919), so cleanup is handled for you once the
worktree's PR lands — you don't need to do it manually.

Reference: <https://doc.rust-lang.org/cargo/reference/profiles.html#incremental>

## 6. sccache

[`sccache`](https://github.com/mozilla/sccache) is a shared compilation
cache — it caches compiler *outputs* keyed by input hash, so identical
compilation units are never recompiled even across different `target/`
directories.

```bash
cargo install sccache
```

Wire it in via environment variable or `.cargo/config.toml`:

```bash
export RUSTC_WRAPPER=sccache
```

```toml
# .cargo/config.toml
[build]
rustc-wrapper = "sccache"
```

**Where it earns its keep:** across branches, across the multiple
`.claude/worktrees/*` this project routinely runs in parallel, and on clean
checkouts — exactly this workspace's multi-worktree development pattern. Two
worktrees building the same shared crate (e.g. `trusty-common`) at the same
version hit the sccache hit-rate hard, avoiding a redundant compile per
worktree.

**Where it helps less:** inside a single tree doing normal incremental
edit-check-edit cycles — cargo's own incremental cache already covers that
case, so sccache adds overhead (hashing, cache lookup) without a
corresponding win.

Adopting `sccache` machine-wide (e.g. in shell rc files, CI images) is an
ops decision with tradeoffs (disk usage, cache invalidation, shared-cache
correctness) — recommend it to the team/operator, don't silently enable it
as a side effect of an unrelated change.

## 7. Restructure Hotspots

When `--timings` (§3) points at a genuine structural bottleneck rather than
something fixable by §4/§5/§6:

- **Split large crates** that create a serial bottleneck in the dependency
  graph — a huge crate blocks everything downstream of it until it finishes;
  this project's own 500-SLOC file cap (see `CLAUDE.md`) is a forcing
  function toward exactly this kind of split.
- **Separate hot-edit code from stable, expensive-to-compile code.** Code you
  edit constantly should live in a small crate that recompiles fast; code
  that rarely changes but is expensive to compile belongs in its own crate so
  editing the hot path doesn't force recompiling the stable part.
- **Limit proc-macros and generated code.** Proc-macro crates are
  disproportionately expensive to compile and often can't be parallelized
  with the code that uses them. Prefer hand-written code or a
  build-script-generated file over a proc-macro when compile time matters
  more than call-site ergonomics.
- **Watch generic instantiation.** Heavily generic code gets monomorphized
  per concrete type at every use site — excessive generics can multiply
  codegen work. Prefer trait objects or fewer type parameters when the
  compile-time cost outweighs the runtime win.
- **Keep build scripts deterministic and cheap.** A `build.rs` that
  re-runs unnecessarily (missing `cargo:rerun-if-changed` directives, or
  doing expensive work every invocation) defeats incremental compilation for
  the whole crate it's attached to.
- **No LTO during iteration.** Link-time optimization is a `--release`-only,
  ship-time concern — never enable it for the dev/inner-loop profile. Reserve
  full optimization and LTO for production release builds, measured
  separately from the dev-loop concerns above.

Reference: <https://doc.rust-lang.org/cargo/reference/profiles.html>,
<https://doc.rust-lang.org/cargo/reference/build-scripts.html>
