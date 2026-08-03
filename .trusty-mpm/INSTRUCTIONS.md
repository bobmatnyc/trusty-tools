# trusty-tools — Claude Code Instructions

Unified Rust workspace consolidating the entire trusty-* AI tooling ecosystem.
21 crates — shared libraries, daemon/MCP servers, the MPM platform, the
control plane, and an orchestrator — all co-located under one Cargo workspace.

## Project Overview

This is a **Rust workspace** (Cargo workspace, resolver v2, glob members
`crates/*`) under the MIT License. Every crate manages its own `version` field independently;
`[workspace.package]` shares `rust-version`, `edition`, `license`, `repository`,
and `authors` but no longer carries a version field (see #343).

**MSRV**: `1.91` — driven by indirect `aws-smithy-*` dependencies that declare
`rust-version = "1.91.1"`; the let-chain stabilisation floor (1.88) is lower.
CI enforces this with `dtolnay/rust-toolchain@1.91`.

## Role & Scope

`trusty-tools` is the **single source of truth** for all trusty-* AI tooling.
It replaces seven formerly separate repos and eliminates the `[patch.crates-io]`
dance for cross-crate development. The authoritative crate list is the
`[workspace.members]` glob in the root `Cargo.toml`; every subdirectory under
`crates/` with a `Cargo.toml` is a member.

Key consumers of the shared libraries:
- **trusty-agents** — agent orchestration platform (lives in `crates/trusty-agents`)
- **trusty-search** — hybrid code search daemon + MCP server
- **trusty-memory** — MCP frontend over the memory palace (storage lives in
  `trusty-common`'s `memory-core` feature)
- **trusty-analyze** — code analysis daemon (complexity, smells, quality metrics)

Work touching a shared crate (e.g. `trusty-common`, `trusty-embedderd`) may
require bumping the dependent crate's version and verifying its tests. Always
run `cargo check` and `cargo test -p <crate>` after modifying a library crate,
then propagate changes to all crates that depend on it before committing.

## Build and Test Commands

🔴 **Single-path workflows — use exactly these commands.**

### Workspace-wide Commands

```bash
# Build all crates (development)
cargo build

# Build all crates (release/optimised)
cargo build --release

# Run all tests
cargo test

# Check (fast compile check, no codegen)
cargo check

# Lint (workspace-wide, all targets)
cargo clippy --workspace --all-targets -- -D warnings

# Format check
cargo fmt --check

# Format and fix
cargo fmt

# Run ONNX-backed integration tests (slow; skipped in CI)
cargo test -- --include-ignored

# Update dependencies (review Cargo.lock diff before committing)
cargo update

# Audit dependencies for known vulnerabilities
cargo audit   # requires: cargo install cargo-audit
```

### Individual Crate Commands

```bash
# Build a single crate (dev)
cargo build -p trusty-search

# Build a single crate (release)
cargo build --release -p trusty-search

# Check a single crate (fastest — no codegen)
cargo check -p trusty-search

# Test a single crate
cargo test -p trusty-search

# Test a single crate with a specific feature
cargo test -p trusty-common --features axum-server

# Test a single test by name within a crate
cargo test -p trusty-search -- my_test_name

# Run a binary from a specific crate
cargo run -p trusty-search -- start
cargo run -p trusty-mpm -- --help

# Build only the binary, not the whole workspace
cargo build --release -p trusty-mpm

# Lint a single crate
cargo clippy -p trusty-search -- -D warnings

# Test a single crate with ignored tests
cargo test -p trusty-embedderd -- --include-ignored

# Run trusty-search performance regression suite (requires daemon + indexed trusty-tools)
cargo test -p trusty-search --test baseline_trusty_tools -- --include-ignored --nocapture
```

### Important: Crate Names vs. Directory Names

**Crate names** match the `name` field in each crate's `Cargo.toml`, not necessarily the directory name.
Most match (e.g. `crates/trusty-search/` → `-p trusty-search`) but note these exceptions:

- `crates/trusty-git-analytics/` → `-p tga` (short name)
- `crates/trusty-agents/` → `-p trusty-agents`

Always verify the `name` field in the crate's `Cargo.toml` if you get a "package not found" error.

## Rust Test Ladder — how much testing this change needs

🔴 **This ladder is the authoritative answer to "how much testing does this
change need" for this repo.** Run the smallest deterministic gate that covers the
change's blast radius — no less, and no more. The framework's phase-entry table
labels a change Low / Normal / High risk; those labels map onto the rungs below
(rungs 1–2 = Low, 3–4 = Normal, 5–6 = High). The framework does not carry a
competing Rust matrix: when the question is *which command to run*, this table
decides.

Required tests stay in the implementation PR (`tm-pr-workflow`, "One Outcome,
One PR"). Name the rung and paste its command in the PR body so a reviewer can
see which rung was actually run.

| # | Change class | Risk | Development proof | PR gate — the command to run | Hardening / release gate |
|---|---|---|---|---|---|
| 1 | Docs, comments, changelog fragments only | Low | Read the rendered file | `bash scripts/check_sld.sh` (plus `check_doc_numbers.sh` / `check_line_cap.sh` if those surfaces were touched). No Cargo test by default. | CI required checks only |
| 2 | Test-only stabilization — flake fix, fixture, test harness | Low | Fail-before / pass-after, repeated: `cargo test -p <crate> <test> -- --exact --nocapture` run ~10× | `cargo fmt --check` && `cargo test -p <crate>`; add `-- --test-threads=1` when the flake is isolation-shaped | `cargo test --workspace` **only** when shared test infrastructure changed |
| 3 | Localized behavior inside one crate | Normal | One targeted regression test that provably fails before the change | `cargo fmt --check` && `cargo check -p <crate>` && `cargo clippy -p <crate> -- -D warnings` && `cargo test -p <crate>` | Workspace gate only when release policy requires it |
| 4 | **Cross-crate change** — public API or shared library (`trusty-common`, `trusty-embedderd`, …) | Normal → High | Targeted regression plus `cargo test -p <lib>` | rung 3 for the library, then `cargo check --workspace` && `cargo test -p <consumer>` for **each direct dependent** | `cargo test --workspace` && `cargo clippy --workspace --all-targets -- -D warnings` at HARDEN/release |
| 5 | Cross-crate contract, persistence, security, process lifecycle, **release tooling** | High | Targeted plus failure-path and concurrency tests | rung 4, plus `cargo test -p <crate> -- --include-ignored` for gated integration coverage, plus an adversarial review round (`code-critic`) | full workspace, `cargo audit`, and for release tooling `scripts/check-publish-ready.sh <crate>` && `scripts/preflight-publish.sh <crate>` |
| 6 | **UI / API surface** — Svelte UIs, MCP tool schemas, HTTP routes | High | Rust crate tests **plus** direct UI/API evidence (curl the route, call the MCP tool, load the page) | rung 3 or 4 for the Rust side, plus `pnpm -C crates/<crate>/ui test` (where the package defines one; otherwise `… build`) and one smoke run of the binary | full product/e2e gate plus `cargo test -- --include-ignored` when hardening |

🔴 **`cargo test --workspace` is not the default inner-loop proof for a localized
change.** It belongs at the hardening boundaries (rungs 4–6). Making every narrow
PR depend on the whole workspace turns unrelated flakes into an issue factory
without adding one line of coverage for your change.

🔴 **Scope down, never scope away.** Choosing a lower rung is a statement about
blast radius, and you must be able to prove it (see the baseline-failure rules
below). It is never licence to make a red gate green by deleting, `#[ignore]`-ing,
`cfg`-gating, `--exclude`-ing, or `--lib`-narrowing coverage. That remains the
hard line it has always been.

**Evidence detail:** a PR body may summarise a **passing** gate as command +
counts + scope — `cargo test -p trusty-mpm — 214 passed, 0 failed` — because the
reviewer's question there is which rung ran, not what scrolled past. Raw output
stays **mandatory** for failures, flakes, performance claims, and disputed
results, and agent-to-PM reporting keeps raw output in all cases
(`BASE-AGENT.md`: never summarise test results in your own words).

### Baseline failures — the Rust specifics

🔴 **Never turn a red gate green by `#[ignore]`-ing, `cfg`-gating, or
`--exclude`-ing a failing test** — prove your diff touches zero files in the
failing crate instead (`git diff --name-only origin/main...HEAD -- <crate>/`).
For the known-environmental flaky tests on this machine (the `trusty-search`
filesystem-watcher tests, `execute_doctor_against_test_daemon`'s timing), the
five-step protocol for telling a pre-existing red from one you caused, and the
exact report-string format: see
[docs/reference/test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).

## Key Conventions

🔴 **Rust issue boundary — what to search by before filing.** Whether a finding
earns an issue at all is decided by the **Ticket-Promotion Gate** in the
framework skill `tm-ticketing` (search first, the five promotion criteria, the
confidence label, outcome-sized granularity, the six-field schema). That gate is
not restated here — read it there. What this repo adds is *what to search by*,
because a Cargo workspace hands you four high-signal keys a generic search
misses:

| Search key | Example | Why it finds the canonical issue |
|---|---|---|
| **Test name** | `execute_doctor_against_test_daemon` | Rust test names are effectively unique and get quoted verbatim in every prior report and CI log |
| **Panic / error text** | `called Option::unwrap() on a None value`, or a `thiserror` Display string | The literal message is what people paste into issue bodies |
| **Affected symbol** | `WatcherManager::reconcile` | Survives the file moves and module splits that break any path-based search — and this repo splits modules constantly under the 500-SLOC cap |
| **Crate** | `-p trusty-search`, `-p tga` | Scopes to the owning workstream; expand abbreviations first (see the table below — `tm` is trusty-memory, not the binary) |

Search **open and recently closed** issues on all four. A repeat failure in the
same crate is almost always another occurrence of an existing canonical issue:
append the run URL, SHA, command, and failure signature to that issue rather than
filing a second one.

🔴 **Why/What/Test doc pattern with proportional depth** — public items carry
documentation proportional to how surprising the code is. The full three-section
pattern (Why/What/Test) is mandatory for API entry points, design-heavy code,
error contracts, safety/TCC behavior, and cross-crate surfaces; a single-line doc
or lightweight pattern suffices for trivial items (simple getters, obvious
one-liners, thin re-exports).

**Full pattern (mandatory for non-obvious code):**

```rust
/// Why: <motivation — the problem this solves, not the mechanics>
/// What: <mechanical description of what the item does>
/// Test: <where coverage lives, or why it is side-effect-only / untestable>
pub fn my_function() { … }
```

**Lighter touch (permitted for self-evident code):**

```rust
/// Returns the user's name.
pub fn name(&self) -> &str { … }
```

**Judgment rule:** If a competent reader's first guess is right, a one-line doc
is complete. Defensive-reasoning paragraphs and issue-history anecdotes belong in
linked ADRs or issues, not inline comments — use `// See <issue-or-adr>` instead.
This keeps the entry point dense and lets detail stay one hop away.

🟡 **Ticket-attributed inline comments** — when you modify a function, class,
or module *because of* a ticket, add a concise inline pointer at the change
site: `// #1234: <one-line reason>`, or `// See #1234` when the ticket title
already says it all. This is the same pointer convention as the judgment rule
above, applied to change attribution instead of design rationale: the ticket
reference is the pointer, never a narrative — one line, not a changelog
embedded in comments. The full reasoning stays in the ticket.

🔴 **No `unwrap()` in library code** — use `?` with `anyhow::Result` for
application/binary code and `thiserror` for library error types. Reserve
`expect()` only for cases that are genuinely programmer errors (invariants that
can never occur at runtime).

🔴 **SLOC file size hard cap (MECHANICALLY ENFORCED, dual-cap since #1131,
TEST_CAP raised #4074):**

| File type | SLOC cap |
|---|---|
| Production source files | **500 SLOC** |
| Test / benchmark files | **3000 SLOC** |

A file is classified as a **test/benchmark file** when ANY of these match:
- basename is exactly `tests.rs`
- basename ends with `_test.rs` or `_tests.rs`
- path contains a `/tests/` directory segment (covers `crates/*/tests/*.rs`
  integration tests AND any `src/**/tests/*.rs` inline test modules)
- path contains a `/benches/` directory segment

All other tracked `.rs` files are **production files**, capped at 500 SLOC.

🔴 As of issue #610 this is mechanically enforced by `scripts/check_line_cap.sh`,
wired into CI and the pre-commit hook — a new tracked file over its cap
**cannot merge**. Never turn this gate green by deleting, `#[ignore]`-ing, or
excluding a file from the count; split it instead (one public module per
logical concept, a thin `mod.rs` re-export facade, sibling single-responsibility
files). See [docs/reference/sloc-cap.md](docs/reference/sloc-cap.md) for the
exact SLOC counting definition, the ratchet-allowlist mechanics
(`.line-cap-allowlist.tsv`, `--update`/`--seed`/`--force-add`), and the
resolved #170/#171/#172 refactor history.

🔴 **`thiserror` for libraries, `anyhow` for binaries** — library crates
(`trusty-common`, `trusty-embedderd`, `trusty-bm25-daemon`, etc.) define structured error enums with
`#[derive(thiserror::Error)]`. Binary and daemon crates use `anyhow::Result`
throughout.

🔴 **Feature flags** — `trusty-common` gates `axum` and `tower-http` behind the
`axum-server` feature flag. Do not add axum as an unconditional dependency in
any library crate. Enable it explicitly in crates that serve HTTP.

🔴 **Common entry point, clean domain demarcation** — Every capability shared across
two or more crates — spawning an external tool (git, gh, tmux, launchctl), building
an HTTP client, resolving a daemon's address, reading a secret or config value,
redacting sensitive output, retrying a fallible call — MUST have exactly one
implementation, living in trusty-common (or the crate that most naturally owns that
domain), that every consumer routes through. Before writing `Command::new(...)`,
`reqwest::Client::builder()`, `std::env::var(...)` for a cross-crate concern, or any
bespoke read-this-config / find-this-daemon / scrub-this-string logic: search for an
existing entry point first (`git grep`, then trusty-common source tree) and extend it
rather than duplicating. A second independent implementation of a shared capability is
a defect, not a convenience — behavior fixes, security patches, and safety guardrails
must land once, not N times, and silent drift between copies is a hidden risk. See the
domain consolidation audit table below for 2026-07-11 baseline status. First enforcement
instances: inference-adapter epic #2400, tmux common entry point #2398/PR #2399.
Scope: this rule governs capabilities shared ACROSS crates. Duplication of a
capability WITHIN a single crate is not covered by it — consolidate that on
its own merits when the drift has caused (or clearly will cause) a real
defect, not by citing this rule (#4058 binary-name-table consolidation).

🟡 **Rust editions** — `edition = "2024"` for `trusty-mpm`, `trusty-mpm-gui`, `trusty-agents`, `trusty-agents-common`, `trusty-agents-local`, and `trusty-code`
(they use let-chains); `edition = "2021"` for all other crates. Check the crate
`Cargo.toml` before assuming an edition.

🟡 **No global state** — all helpers are free functions or small structs. No
`lazy_static!` or `once_cell::sync::Lazy` except the tracing subscriber (which
uses `try_init` to be idempotent across test binaries).

🟡 **Logs to stderr** — `init_tracing` always writes to stderr so stdout stays
clean for MCP JSON-RPC framing. Never log to stdout in a daemon or MCP server.

🟡 **Ignore-tagged integration tests** — ONNX-backed embedder tests are marked
`#[ignore]` to keep CI fast. Run with `cargo test -- --include-ignored` when
you need local validation against the model.

🟢 **Workspace dependency sharing** — all shared external crates (`anyhow`,
`serde`, `tokio`, `axum`, etc.) are declared once in `[workspace.dependencies]`
in the root `Cargo.toml` and referenced as `dep = { workspace = true }` in
crate manifests. Never pin a dependency locally if it is already in the
workspace table.

🟢 **Internal path deps** — reference sibling crates as:
```toml
trusty-common = { workspace = true }
```
The workspace manifest declares the path so every member resolves from the
in-tree source automatically. The `[patch.crates-io]` block in the root
`Cargo.toml` also redirects any crates that still reference the old published
versions by version number.

## Git Tag / Release Convention

> **Full release workflow and macOS critical notes:** see [docs/reference/release-workflow.md](docs/reference/release-workflow.md).

### Quick Release Steps

1. Bump the crate version in `crates/<name>/Cargo.toml`.
   - Shortcut: `scripts/bump-version.sh <crate-dir> <major|minor|patch>` reads the
     current version, computes the next semver, edits the package version in
     place, syncs the root `Cargo.lock` (`cargo update -p <package-name>
     --precise <next-version>`, resolving `<package-name>` from the crate's
     `[package] name` field so it works even when that differs from the
     crates/ directory name, e.g. `tga`) so the release PR is `--locked`-clean
     on the first CI run (issue #3199), assembles the crate's
     `changelog.d/` fragments into a `## [<version>]` `CHANGELOG.md` section
     (via `scripts/assemble-changelog.sh`, issue #4476), and **prints** the exact
     `git tag` / `git push` commands for you to run. It never tags or pushes —
     the human stays in the loop per the manual-tag convention. It honours the
     `trusty-git-analytics` tag-prefix gotcha automatically (tags are
     `trusty-git-analytics-v*`, not `tga-v*`).
2. Update any dependent crates that pin that version.
3. Run `cargo test -p <name>` and `cargo clippy --workspace -- -D warnings`.
4. Commit the version bump.
5. Create the tag: `git tag <crate-name>-v<version>`.
6. Push the tag: `git push origin <crate-name>-v<version>`.
7. Run `scripts/check-publish-ready.sh <crate>` (or `make publish-check CRATE=<crate>`) —
   must pass (publish only from merged main; never an unmerged branch). See
   issue #2227; escape hatch `ALLOW_UNMERGED_PUBLISH=1` is for rare, deliberate
   use only.
7b. Run `scripts/preflight-publish.sh <crate>` — must pass immediately before
    `cargo publish`. Rule: **publish only from merged main, as bobmatnyc.**
    Checks HEAD == origin/main exactly, the active `gh auth status` account is
    `bobmatnyc`, the tree is clean, and the target version isn't already live
    on crates.io. This closes the gap behind the 2026-07-08 incident, where a
    crate was published from an unmerged branch under the wrong gh account and
    burned crates.io version 0.22.0 with fix-less content.
8. Publish: `cargo publish -p <crate-name>` (or `SKIP_UI_BUILD=1 cargo publish` for UI-embedding crates).
9. Build and install: `cargo install --path crates/<dir> --locked`.

### Per-PR Changelog Fragment (framework default — issue #4476, supersedes the shared-`[Unreleased]` convention)

🔴 **Every PR that touches a crate's `src/**` adds a changelog FRAGMENT file to
that crate, in the same PR. Never edit a crate's `CHANGELOG.md` by hand.**

```
crates/<crate>/changelog.d/<issue-or-pr-number>-<short-slug>.md
```

```
Fixed

- pm_guard no longer scans quoted content (closes [#2741](https://github.com/bobmatnyc/trusty-tools/issues/2741))
  - indented sub-bullets are preserved verbatim
```

- **Line 1 is the category:** `Breaking` | `Added` | `Fixed` | `Performance` |
  `Changed` | `Removed` | `Security` | `Documentation` (the same groups the
  changelogs already use).
- **Everything after it is the bullet text**, copied through verbatim. Match the
  crate CHANGELOG's existing style.
- **The file must sit directly in `changelog.d/`.** A nested one (`changelog.d/sub/…`)
  is rejected at release time; `changelog.d/README.md` is the tracked directory
  placeholder and is never treated as a fragment.
- **Preview what the next release will say:**
  `bash scripts/assemble-changelog.sh <crate-dir> --stdout`.
- **The filename's leading number is what makes this collision-free.** Two
  concurrent PRs add two differently-named files, so git never sees a conflict.
  That is the entire point: on 2026-07-31 five concurrent trusty-mpm PRs
  (#4463–#4475) each wrote a bullet into the shared `## [Unreleased]` section and
  every merge forced the next PR to rebase and hand-resolve it (#4399 burned
  three such rounds).
- Docs-only, CI-only, test-only and `testdata/` PRs may skip this step. A PR that changes
  crate source and lands with no fragment is a **review-gate failure**, the same
  tier as a failing `cargo test`/`cargo clippy` gate — and it is now also a CI
  failure (`.github/workflows/changelog-fragment.yml` →
  `scripts/check_changelog_fragment.sh`). No "trivial change" exception.

**Fragments are the source of truth for the unreleased set.** `CHANGELOG.md`
carries released version sections only — no `## [Unreleased]` heading between
releases. At release time `scripts/bump-version.sh` calls
`scripts/assemble-changelog.sh <crate-dir> <version>`, which groups the crate's
fragments by category, writes one `## [<version>] — <date>` section, and deletes
the consumed fragments in the same operation. Preview the pending set any time
with `scripts/assemble-changelog.sh <crate-dir> --stdout`.

**git-cliff no longer touches `CHANGELOG.md`.** `scripts/generate-changelog.sh`
is deleted; it ran `git cliff --unreleased --prepend`, which blindly stacked a
fresh `## [Unreleased]` on top of the hand-written one — the defect #2793 tracks
and the reason `bump-version.sh` used to carry a duplicate-heading stopgap. Both
are gone: there is exactly ONE mechanism that writes a crate changelog, and it
is the assembler. `cliff.toml` stays, scoped to rendering the **GitHub Release
body** in `.github/workflows/release.yml` (`--latest --strip all`), which never
writes `CHANGELOG.md`.

Transitional note: PRs opened before #4476 landed wrote into the shared
`## [Unreleased]` section and were not converted. The gate accepts a
`CHANGELOG.md` edit as evidence so they stay green, and the assembler refuses to
run while a leftover `## [Unreleased]` heading survives — fold those bullets into
the section being cut at the next release of that crate.

🟢 **`tga` tag aliases (issue #1128):** `trusty-git-analytics` publishes to
crates.io under the short package name `tga`. The binary-release workflow
(`.github/workflows/release.yml`) accepts **both** tag forms — `tga-v<version>`
and `trusty-git-analytics-v<version>` — and they resolve to the **same** build
config (CARGO_PKG=`tga`, binary `tga`) and the same Homebrew formula. The parse
step canonicalizes the `tga` prefix to `trusty-git-analytics` for the config map,
the homebrew-bump job, and changelog path scoping, while the changelog
`--tag-pattern` keys off the literal pushed tag so release notes match either
form. Use whichever you prefer; the documented `<crate-name>-v<version>`
convention (i.e. `tga-v<version>`, matching the abbreviation table) works.

🔴 **CRITICAL macOS note:** Never use `cp` to install release binaries on macOS — always use `cargo install`. See release workflow reference for the detailed explanation.

### macOS TCC (Full Disk Access / App Data) and daemon restart (issues #873, #2558, #534)

🟢 **Scope split — read before re-granting anything:** `trusty-search` (and
other external-volume daemons) needs **Full Disk Access**; `trusty-mpm` / `tm`
needs the separate **App Data** TCC category only — it never needs, and should
never be granted, Full Disk Access. Every `cargo install` mints a new cdhash,
invalidating the previous grant; `tctl install <crate>` re-signs with a stable
Developer-ID identity so the grant persists across reinstalls.

See [docs/reference/release-workflow.md](docs/reference/release-workflow.md)
for certificate setup, signed-install scripts, the restart playbook
(`launchctl bootout`, never `cp`, never trust a bare `GET /health`), and
orphan-listener verification (issues #2486, #4230).

## Cross-Crate Development Workflow

Because all crates are in the same workspace, the `[patch.crates-io]` dance
that was required with separate repos is no longer necessary for active
development. Cargo resolves internal crates via path automatically.

When you modify a library crate:
1. Edit the crate under `crates/<lib>/`.
2. Run `cargo check` to catch compilation errors across the entire workspace.
3. Run `cargo test -p <lib>` for the modified library.
4. Run `cargo test -p <consumer>` for each crate that depends on the library.
5. Commit all changes together — workspace builds are atomic.

When publishing a crate to crates.io:
- The path dep in `[workspace.dependencies]` coexists with the version field,
  so `cargo publish` sees the version and uploads correctly.
- The `[patch.crates-io]` block in the root `Cargo.toml` ensures the in-tree
  crates are preferred during local builds even if a published version exists.

## Parallel Worktree Discipline

**CRITICAL RULES FOR CONCURRENT SESSIONS:**

🔴 **SOURCE OF TRUTH = `origin/main:HEAD`.** Local `main` may be stale. **Always `git fetch origin main` and branch worktrees off `origin/main`** (not local main). Stale local main has caused lost commits and missed features. This is not optional.

🔴 **The main checkout is inspection-only.** From the repo root
(`/path/to/trusty-tools/`), the only allowed operations are read-only: `git status`,
`git log`, `git diff`, `git show`, file reads. **FORBIDDEN**: edits, `git reset --hard`,
`git checkout .`, `git stash`, `git restore .`, `cargo build`/`cargo test`,
`sed`/`awk`/`patch`, or any command that mutates the working tree, index, or `target/`.

🔴 **All write-side work happens in a dedicated git worktree branched off
`origin/main`.** Provision one before starting any edit, build, or test:

```bash
git fetch origin main
git worktree add -b <feature-or-fix-branch> \
                  .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>
# … edit, build, test, commit, push from here …
```

**End-to-end delivery chain:** accepted outcome → optional issue → worktree
branch → one cohesive PR → applicable Rust gates → trusty-review gate →
squash-merge → worktree cleanup. The framework skill `tm-pr-workflow` owns the
full sequence and the optional-issue rule; this file adds only the Rust-specific
gates (see the Rust Test Ladder above). *(The pointer here used to name
`.claude-mpm/INSTRUCTIONS.md`, a path that has never existed in this repo — the
tracked project-instruction host is this file, `.trusty-mpm/INSTRUCTIONS.md`,
and the delivery chain itself now lives in `tm-pr-workflow`.)*

🔴 **A worktree is a writer; the branch is the workstream.** The durable unit is
the **branch** — one branch per workstream, and a session owns exactly one
workstream. A worktree is only the checkout that lets you write to that branch:
ephemeral, disposable, and recreatable at any time with `git worktree add`.
Never treat a worktree as the thing being preserved; losing one loses nothing
the branch does not still hold.

What follows from that:

- **One branch and worktree per independently reviewable PR outcome** — not per
  ticket, per refactor step, or per experiment. Several related tickets may
  share one worktree when a single coherent change satisfies them
  (`Closes #A`, `Closes #B`).
- **Everything that outcome owes stays in the same worktree and PR:** the
  implementation, its regression tests, necessary local refactoring, docs, the
  changelog fragment, and in-scope review fixes. Do not open a second worktree
  for the tests you still owe the first one.
- **Experiments stay session-local.** Promote an experiment to a branch and
  worktree only once its result is accepted for implementation.
- **Cleanup:** `git worktree remove --force <path>` once the PR has merged, then
  `git branch -D <branch>` and `git push origin --delete <branch>` — the branch
  goes last, because until the squash-merge has landed it is the only durable
  copy of the workstream.

🟡 **If you absolutely must run a command from the main checkout** — for
example `cargo install --path crates/<name> --locked` after a merge —
stash first, operate, then restore:

```bash
git -C /path/to/main-checkout stash push -u \
    -m "claude: pre-op-safety $(date +%s)"
# … do the op …
git -C /path/to/main-checkout stash pop
```

Surface the stash name in your report if popping fails so the human can
restore manually.

🟡 **`cargo install` from a worktree, not the main checkout.** The preferred
pattern for installing a freshly-built binary onto your PATH is:

```bash
cargo install --path .claude/worktrees/<dirname>/crates/<name> --locked
```

Cargo writes atomically to a temp file and renames into `~/.cargo/bin/`,
which keeps the macOS kernel's cdhash cache consistent (see the
release-workflow note above). The main checkout never needs to be involved.

🟢 **Subagents inherit these rules.** Every `Agent`/`Task` dispatch prompt
**must** name the exact worktree path the agent should operate from and forbid
leaving that worktree into the main checkout, `git reset --hard`, `git checkout .`,
and `git stash` against the main checkout, and touching files outside the assigned worktree.

The pattern of instructing an agent to "operate from the main checkout" is banned.
QA agents get their own worktree (`.claude/worktrees/qa-<ticket-or-pass>`) just
like engineering agents.

🟢 **Worktree cleanup is safe.** `git worktree remove --force <path>` deletes
the worktree directory but never the main checkout. Use `git branch -D <branch>`
and `git push origin --delete <branch>` to clean up refs after a squash-merge.

> **Extended discipline rationale and cleanup details:** see [docs/reference/worktree-discipline.md](docs/reference/worktree-discipline.md).

## Abbreviations & Aliases

When the user (or any agent) refers to a crate by abbreviation, resolve it using this table before taking any action.

| Abbreviation | Full crate name | Cargo package flag | Directory |
|---|---|---|---|
| `tga` | trusty-git-analytics | `-p tga` | `crates/trusty-git-analytics/` |
| `tm` | trusty-memory | `-p trusty-memory` | `crates/trusty-memory/` |
| `ts` | trusty-search | `-p trusty-search` | `crates/trusty-search/` |
| `tc` | trusty-common | `-p trusty-common` | `crates/trusty-common/` |
| `ta` | trusty-analyze | `-p trusty-analyze` | `crates/trusty-analyze/` |
| `mpm` | trusty-mpm | `-p trusty-mpm` | `crates/trusty-mpm/` |
| `tagent` or `t-agents` | trusty-agents | `-p trusty-agents` | `crates/trusty-agents/` (bin: `tagent`) |
| `t-agents-common` | trusty-agents-common | `-p trusty-agents-common` | `crates/trusty-agents-common/` |
| `t-agents-local` | trusty-agents-local | `-p trusty-agents-local` | `crates/trusty-agents-local/` |
| `tcode` | trusty-code | `-p trusty-code` | `crates/trusty-code/` |
| `tctl` | trusty-installer | `-p trusty-installer` | `crates/trusty-installer/` |

These abbreviations apply everywhere: ticket descriptions, build commands, references in conversation. Always expand before running `cargo` commands.

> **Auto-resolution:** When connected to trusty-memory MCP, call `get_prompt_context()` at the start of each turn to load current aliases and conventions. Pass a `query` string to filter to relevant facts only.

## Development Environment

### Required Tools

- **Rust**: `rustup` with the toolchain pinned to MSRV `1.91` or later.
  Install: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Node / pnpm**: only needed if working on the Svelte UIs embedded in
  `trusty-search` or `trusty-memory`. Install pnpm via `npm i -g pnpm`.
- **Git**: standard; the workspace uses git tags for per-crate releases.

### Environment Variables

> **Full environment-variable reference:** see [docs/reference/environment-variables.md](docs/reference/environment-variables.md).

Key variables:
- `OPENROUTER_API_KEY` — LLM chat via OpenRouter
- `TRUSTY_LLM_MODEL` — LLM model for deep-analysis pass (default: `openai/gpt-4o-mini`)
- `RUST_LOG` — Tracing filter (e.g., `RUST_LOG=debug`)
- `SKIP_UI_BUILD` — Set to `1` to skip Svelte UI build in `build.rs`
- `TRUSTY_NO_KG` — Set to `1` to skip knowledge-graph construction by default

### IDE Setup

> **Full IDE setup reference:** see [docs/reference/ide-setup.md](docs/reference/ide-setup.md).

Quick: VS Code needs `rust-analyzer` + `Even Better TOML` extensions; RustRover auto-detects the workspace.

### Running Individual MCP Servers Locally

> **Detailed MCP server examples and wiring:** see [docs/reference/running-mcp-servers.md](docs/reference/running-mcp-servers.md).

Quick: `RUST_LOG=info cargo run -p trusty-search -- start` (daemon), `cargo run -p trusty-search -- serve` (MCP stdio mode).

## Common Pitfalls — Quick Checklist

For extended explanations, see [docs/reference/common-pitfalls.md](docs/reference/common-pitfalls.md).

- **Library error handling:** use `thiserror`, not `unwrap()` in libraries
- **Daemon stdout:** never log to stdout in daemons or MCP servers
- **Axum in libraries:** gate behind `axum-server` feature flag
- **Shared crate changes:** always run `cargo check` + tests for all dependents
- **SLOC cap:** respect 500/3000 SLOC limits (prod/test); use `bash scripts/check_line_cap.sh`
- **UI build:** install pnpm or set `SKIP_UI_BUILD=1` before `cargo build`
- **Patch tables:** put all `[patch.crates-io]` in root `Cargo.toml` only
- **MSRV drift:** prefer stable channel toolchains; don't break `rust-version = "1.91"`
- **Edition mismatch:** 2024 crates (mpm, agents, mpm-gui, agents-common, agents-local) may use let-chains; 2021 crates cannot

## Reference Documentation

Full-length reference materials for less-frequent lookups:

- **Code structure & crate map:** [docs/reference/crate-map.md](docs/reference/crate-map.md)
- **Documentation layout conventions:** [docs/reference/documentation-layout.md](docs/reference/documentation-layout.md)
- **Former repos (monorepo history):** [docs/reference/former-repos.md](docs/reference/former-repos.md)
- **Release workflow (with macOS signing details):** [docs/reference/release-workflow.md](docs/reference/release-workflow.md)
- **Worktree discipline (extended rationale):** [docs/reference/worktree-discipline.md](docs/reference/worktree-discipline.md)
- **Common pitfalls (detailed explanations):** [docs/reference/common-pitfalls.md](docs/reference/common-pitfalls.md)
- **Environment variables (full table):** [docs/reference/environment-variables.md](docs/reference/environment-variables.md)
- **IDE setup (detailed):** [docs/reference/ide-setup.md](docs/reference/ide-setup.md)
- **Running MCP servers (examples & wiring):** [docs/reference/running-mcp-servers.md](docs/reference/running-mcp-servers.md)
- **Spec-Linked Documentation (SLD) policy:** [DOC-38](docs/specs/spec-linked-documentation.md) — the standard for declaring source↔spec references; enforced by `scripts/check_sld.sh`.
- **HTTP trust-boundary threat model:** [docs/reference/threat-model.md](docs/reference/threat-model.md) — per-daemon bind/guard/proxy compliance inventory for the loopback-only doctrine ([ADR-0018](docs/adr/0018-loopback-only-doctrine.md)).

## Domain Consolidation Audit

**Baseline audit 2026-07-11** (session d72fb4a3-f9ff-4394-b148-8200d17a2d5a). Update verdicts
as consolidations land. Reference: common-entry-point principle above.

| Domain | Verdict | Scale | Status |
|--------|---------|-------|--------|
| HTTP clients (reqwest) | FRAGMENTED | 150+ builder sites incl. 20+ inside trusty-common | backlog |
| git invocations | FRAGMENTED | ~90 production spawn sites, 7 crates | backlog (after tmux pattern proves) |
| Shared env-var access | FRAGMENTED | OPENROUTER_API_KEY ×22 files/8 crates; GITHUB_TOKEN ×13/6 | partially covered by epic #2400 (#2401) |
| gh CLI | FRAGMENTED | 3 wrappers (trusty-agents ticketing/gh_cli most complete) | backlog |
| Config-file loading | FRAGMENTED | 5 implementations (2 pairs within single crates) | backlog |
| Secret redaction | FRAGMENTED (security) | 4 rule sets (3 inside trusty-mpm) | partially covered by #2401 redact_secret |
| launchctl | SCATTERED | shared LaunchdConfig exists; 9 bypass sites/4 crates | backlog |
| Daemon addr discovery | SCATTERED | common resolver exists; 3 daemons re-implement | backlog |
| Daemon PID discovery | FRAGMENTED | 3 copy-pasted find_daemon_pids() | backlog |
| tmux (trusty-mpm) | SCATTERED→fixing | 19 sites | in flight: #2398/PR #2399 |
| LLM inference | FRAGMENTED→fixing | 6 bespoke clients | epic #2400 |
