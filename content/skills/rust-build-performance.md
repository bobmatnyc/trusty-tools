---
name: rust-build-performance
description: "Rust build performance: scoped checks, timings, dependency hygiene, shared caches, bounded output, and Apple Silicon scheduling. Use for slow builds or excessive logs."
user-invocable: false
metadata:
  version: "1.2.0"
category: agent-reference
effort: low
---
# Rust Build Performance

The big wins live in the **development loop** and the **dependency graph**,
not in compiler flags. Optimize those first; flags and LTO are a last resort
for measured hotspots.

## 1. Core Principle

Inner loop, in order:

1. `cargo check -p <crate>` while writing — catches type and borrow errors,
   never blocks on codegen.
2. `cargo test -p <crate>` for the crate you touch.
3. A full build only for a runnable binary, a PR, or a release.

**Name the objective first**: cold full build, warm incremental rebuild, or
test-edit-run latency. They want different fixes. Compare variants under the
same objective, toolchain, features, profile, target dir and cache state, and
repeat each run; one timing is an anecdote.

## 2. `cargo check` While Coding

`cargo check` skips codegen. Scope it with `-p <crate>`; widen only for
cross-crate diagnostics.

**`cargo check` without `-p` covers only the workspace's `default-members`**
(the root package when there is one). Use `--workspace` when the claim is
"the whole workspace type-checks".

The project's risk/stage test ladder defines the shipping gate: its crate,
consumer and release checks, and doc gates for prose-only changes. Do not add
a workspace build just because work is ready to land.

**Before a concurrent build**, name the assigned `CARGO_TARGET_DIR`, job
limit, profile/features and lock-wait bound in the brief. Use the existing
allocator if there is one; otherwise coordinate ownership with the PM. On a
lock wait, identify the holder and report the bound; never kill another
session's build or start a duplicate. Preserve the assigned cache across
compatible runs. See #8021.

## 3. Measure Before Tuning

```bash
cargo build --timings   # → target/cargo-timings/cargo-timing.html
```

The report separates codegen, dependency waits and build-script time. Find
serial chains, slow proc-macros and needless build-script reruns before tuning.
Reference: <https://doc.rust-lang.org/cargo/reference/timings.html>

## 4. Reduce Dependency / Feature Load

Dependencies and proc-macros dominate clean-build time. Audit the graph:

```bash
cargo tree --duplicates       # each extra version is another compile
cargo tree --edges features   # which features, pulled in by what
```

- Disable default features you do not need
  (`serde = { version = "1", default-features = false, features = ["derive"] }`).
- Drop heavyweight dependencies with marginal value; unify duplicate versions.
- Compile only what you exercise: `-p <crate>` and feature flags.
- Features are additive and unified per resolver: with resolver 2, a feature
  enabled by any member in the build is on for every user of that crate in that
  build. Check what a command actually builds with
  `cargo tree -p <crate> -e features -i <dep>`; a stray feature that builds a
  second copy of a dependency is a measurable cost.
- Never pin locally a dependency already in `[workspace.dependencies]`; a
  duplicate defeats version unification.

**Dev-dependency edges are compile-graph edges.** Separate crates exist for a
cheaper compile; a test-only edge that welds two crates' graphs defeats that.

- Never add a workspace crate to `[dev-dependencies]` or `[build-dependencies]`
  when it is absent from the consumer's normal dependency tree.
- A test needing two crates lives in a `publish = false` test crate that
  depends on both, or in the crate that already depends on the other.
- Re-declaring a normal dependency under `[dev-dependencies]` with extra
  features compiles it a second time. Do it only when the feature must never
  reach production, and say why in a manifest comment.
- Check without building: `cargo tree -p <crate> -e dev --prefix none` adds no
  workspace crate that `-e normal` lacks.

See #8341. Reference: <https://doc.rust-lang.org/cargo/reference/features.html>

## 5. Preserve Incremental Compilation

The `dev` profile is incremental; cargo reuses per-crate artifacts in
`target/`. Protect that cache:

- Do not delete `target/` mid-task (forces a cold rebuild of everything).
- Do not churn `RUSTFLAGS`, feature sets or toolchains on the same crate.

A lighter debug profile (`debug = "line-tables-only"`) is a **local,
uncommitted** override; never commit profile changes to the workspace root
without team approval.

**Worktree caches:** a fresh worktree has an empty `target/`. An assigned
shared `CARGO_TARGET_DIR` reuses registry dependencies; path crates still
rebuild because their absolute source paths differ. Shared builds serialize on
Cargo's lock: apply the ownership and wait bounds in §2. `rust-delivery-workflow`
§4 documents setup; use the `tm doctor` `rust_build_env` prefix, not a
hand-assembled one.

Never delete live build caches. The PM reclaims merged, unowned worktrees via
`tm session prune-worktrees --merged-prs --force` (#5791, #2919); it is not
automatic and must preserve unsaved work and live owners.

## 6. sccache

[`sccache`](https://github.com/mozilla/sccache) caches compiler outputs by
input hash, across `target/` directories. It is **conditional**: adopt it from
evidence, not by default.

- It cannot cache incremental artifacts. Workspace path crates build
  incrementally under `dev`, so they get no cross-worktree hit unless the
  wrapped build sets `CARGO_INCREMENTAL=0` (which gives up the incremental win).
- Non-incremental external dependencies are identical across worktrees with the
  same `Cargo.lock` and can hit freely.
- Path-dependent compiler inputs defeat hits; check `sccache --show-stats` for
  hits, misses and non-cacheable calls, and set `SCCACHE_BASEDIRS` when
  checkouts live under different roots.
- Compare before and after under the same objective (§1). No measured gain, no
  adoption. Inside one tree doing edit-check cycles it only adds overhead.

Enabling it machine-wide (shell rc, CI images) is an operator decision with
disk and cache-correctness tradeoffs: recommend it, never enable it as a side
effect of another change.

## 7. Restructure Hotspots

When `--timings` shows a structural bottleneck that §4 to §6 cannot fix:

- **Split crates on the dependency graph, not on file size.** A file-size cap
  (such as this project's SLOC cap) splits modules, not compilation units: the
  crate is still one unit, so a module split buys readability, not build speed.
  Only a new crate lets downstream crates start earlier or lets edits skip
  recompiling stable code.
- Keep hot-edit code in a small crate, and stable expensive code in its own.
- **Thin generic wrapper over a concrete implementation.** A generic function
  is compiled per concrete type at each use site. Make the public function a
  thin generic shim (`fn open(p: impl AsRef<Path>) { open_inner(p.as_ref()) }`)
  and put the body in a non-generic function, so it compiles once. Find the
  worst offenders with `cargo llvm-lines` (copies and lines per function) and
  fix those, not every generic. Trait objects trade runtime cost for fewer
  copies; measure that too.
- **Measure generated code rather than banning proc-macros.** A proc-macro
  crate is not slow by nature. Read `--timings` for the macro crate's compile
  time, and `cargo llvm-lines` or `cargo expand` for how much code its users
  get. Replace a macro only when the numbers say it costs more than the
  hand-written or build-script alternative.
- **Keep `build.rs` cheap and deterministic.** Emit `cargo:rerun-if-changed`;
  a script that always reruns invalidates the crate's incremental cache.
- **No LTO during iteration.** It is a release-profile concern; measure it
  separately from the dev loop.

Reference: <https://doc.rust-lang.org/cargo/reference/profiles.html>

## 8. Build Output Volume

Run `cargo check -p <crate>` before build or clippy. Use
`--message-format=short` for one-line diagnostics and `-q` to suppress
progress. Redirect logs; report exit status and final result lines per
BASE-AGENT's Gate Output rule. Read failure details only on non-zero. Never
pipe a gate into `tail`: it hides the failure status.

```bash
cargo check -q -p <crate> --message-format=short > <scratchpad>/check.log 2>&1
echo "EXIT=$?"
```

Keep `--no-fail-fast` on tests so one failing target cannot hide later ones.

## 9. macOS / Apple Silicon

- **Never `nice` a build.** Cap concurrency with `CARGO_BUILD_JOBS`. On a
  measured Apple Silicon host `nice` parked the work on efficiency cores while
  performance cores sat idle.
- **First-launch scan.** macOS scans each new executable on first launch, and a
  copy at a new path is scanned again. Run test binaries from their cargo paths
  (integration tests locate the crate's binary through `CARGO_BIN_EXE_*`), and
  never copy them into fresh temp directories. See the nextest macOS page:
  <https://nexte.st/docs/installation/macos/>
