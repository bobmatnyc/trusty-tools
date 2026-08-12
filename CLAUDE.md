# trusty-tools — Claude Code Instructions

Unified Rust workspace consolidating the entire trusty-* AI tooling ecosystem.
21 crates — shared libraries, daemon/MCP servers, the MPM platform, the
control plane, and an orchestrator — all co-located under one Cargo workspace.

## Project Overview

This is a **Rust workspace** (Cargo workspace, resolver v2, glob members
`crates/*`) under the MIT License. Every crate manages its own `version` field independently;
`[workspace.package]` shares `rust-version`, `edition`, `license`, `repository`,
and `authors` but no longer carries a version field (see #343).

**MSRV**: `1.94`, enforced in CI with `dtolnay/rust-toolchain@1.94`. The
`aws-config` / `aws-sdk-bedrockruntime` floor that drives it, and the per-crate
edition policy, are in
[ADR-0029](docs/adr/0029-msrv-1-94-and-edition-policy.md).

## Role & Scope

`trusty-tools` is the **single source of truth** for all trusty-* AI tooling.
The authoritative crate list is the `[workspace.members]` glob in the root
`Cargo.toml`; every subdirectory under `crates/` with a `Cargo.toml` is a
member. Per-crate purposes are in
[docs/reference/crate-map.md](docs/reference/crate-map.md); the seven repos this
monorepo replaced are in
[docs/reference/former-repos.md](docs/reference/former-repos.md).

## Build and Test Commands

🔴 **Single-path workflows — use exactly these commands.** Scope to the crate
you changed; a bare workspace run is a hardening gate, not an inner loop.

```bash
cargo build                                            # build all crates (dev)
cargo check -p <crate>                                 # fastest — no codegen
cargo test -p <crate>                                  # test one crate
cargo clippy -p <crate> --all-targets -- -D warnings   # lint one crate
cargo fmt                                              # format (--check to verify only)
```

For any other invocation — release builds, feature-gated tests,
`--include-ignored` / ONNX tests, a single test by name, the `trusty-search`
performance regression suite, `cargo update` / `cargo audit`, or running a
crate's binary — call `Skill(skill="cargo-commands")` rather than guessing.

🟡 **Crate name ≠ directory name.** `-p <crate>` takes the `name` field from the
crate's `Cargo.toml`, not the directory. Most match; the exceptions are in the
Abbreviations & Aliases table below (notably `crates/trusty-git-analytics/` →
`-p tga`). On "package not found", read the crate's `Cargo.toml`.

## Rust Test Ladder — how much testing this change needs

🔴 **This ladder is the authoritative answer to "how much testing does this
change need" for this repo.** Run the smallest deterministic gate that covers the
change's blast radius — no less, and no more. The framework's Low / Normal / High
risk labels map onto the rungs below (1–2 = Low, 3–4 = Normal, 5–6 = High); when
the question is *which command to run*, this table decides.

Required tests stay in the implementation PR (`tm-workflow`, "One Outcome,
One PR"). Name the rung and paste its command in the PR body so a reviewer can
see which rung was actually run.

| # | Change class | Risk | PR gate, in short |
|---|---|---|---|
| 1 | Docs, comments, changelog fragments only | Low | Doc gates only (`check_sld.sh`, plus doc-numbers / line-cap if touched). No Cargo test by default. |
| 2 | Test-only stabilization — flake fix, fixture, test harness | Low | `fmt --check` + `test -p <crate>`, with the flake re-run ~10× |
| 3 | Localized behavior inside one crate | Normal | `fmt --check` + `check` + `clippy` + `test`, all `-p <crate>`, plus one regression test that provably failed before |
| 4 | **Cross-crate change** — public API or shared library (`trusty-common`, `trusty-embedderd`, …) | Normal → High | Rung 3 on the library, then `check --workspace` + `test -p <consumer>` for **each direct dependent** |
| 5 | Cross-crate contract, persistence, security, process lifecycle, **release tooling** | High | Rung 4, plus `--include-ignored` integration coverage, failure-path/concurrency tests, and a `code-critic` round |
| 6 | **UI / API surface** — Svelte UIs, MCP tool schemas, HTTP routes | High | Rung 3 or 4 for the Rust side, plus the UI package's own test/build and one binary smoke run — with direct UI/API evidence, not just crate tests |

The exact command chain for each rung, its development-proof step, and its
hardening/release gate are in
[docs/reference/test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).
Read that page's rung entry before running the gate, and paste the command you
actually ran into the PR body.

🔴 **`cargo test --workspace` is not the default inner-loop proof for a localized
change.** It is keyed to the stage, not the rung: it belongs at the publish
boundary, and rungs 4–6 are the ones that reach it — a rung-4 PR does not owe a
workspace run to merge. Making every narrow PR depend on the whole workspace
turns unrelated flakes into an issue factory without adding one line of coverage
for your change.

🔴 **Scope down, never scope away.** Choosing a lower rung is a statement about
blast radius, and you must be able to prove it (see the baseline-failure rules
below). It is never licence to make a red gate green by deleting, `#[ignore]`-ing,
`cfg`-gating, `--exclude`-ing, or `--lib`-narrowing coverage. That remains the
hard line it has always been.

**Evidence detail:** a PR body may summarise a **passing** gate as command +
counts + scope; raw output stays **mandatory** for failures, flakes, performance
claims, and disputed results. Full rule in
[docs/reference/test-ladder-baseline.md](docs/reference/test-ladder-baseline.md)
("How Much Gate Output the PR Body Owes").

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

🔴 **Rust issue boundary — search before filing.** Whether a finding earns an
issue at all is decided by the **Ticket-Promotion Gate** in the framework skill
`tm-ticketing` — read it there. What this repo adds is *what to search by*: the
**test name**, the **panic / error text**, the **affected symbol**, and the
**crate**. Search open and recently closed issues on all four. What to do with a
hit — `COMMENT`, `REOPEN`, `NEW REGRESSION`, or `NO TICKET` — is `tm-ticketing`'s
disposition to make, not automatically an append (#5202). Rationale per key:
[docs/reference/issue-search-keys.md](docs/reference/issue-search-keys.md).

🔴 **Why/What/Test doc pattern with proportional depth** — public items carry
documentation proportional to how surprising the code is:

```rust
/// Why: <motivation>   /// What: <mechanics>   /// Test: <where coverage lives>
```

The full three-section pattern is mandatory for API entry points, design-heavy
code, error contracts, safety/TCC behavior, and cross-crate surfaces. A
single-line doc suffices for trivial items (simple getters, obvious one-liners,
thin re-exports) — if a competent reader's first guess is right, one line is
complete. Defensive-reasoning paragraphs and issue-history anecdotes belong in
linked ADRs or issues, not inline comments; use `// See <issue-or-adr>`.
Worked examples and the four-axis model: `Skill(skill="documentation-style")`.

🟡 **Ticket-attributed inline comments** — when a ticket drives a change, leave a
pointer at the change site: `// #1234: <one-line reason>`, or `// See #1234`. One
line, never a narrative; the reasoning stays in the ticket.

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

Comments, doc comments (`//`, `///`, `//!`, `/* … */`), and blank lines do
**not** count toward the cap — only non-comment code lines in tracked `.rs`
files do (e.g. `crates/trusty-common/src/lib.rs` is 809 raw lines but 113
SLOC). Exact counting definition:
[docs/reference/sloc-cap.md](docs/reference/sloc-cap.md).

A file is classified as a **test/benchmark file** when ANY of these match:
- basename is exactly `tests.rs`
- basename ends with `_test.rs` or `_tests.rs`
- path contains a `/tests/` directory segment (covers `crates/*/tests/*.rs`
  integration tests AND any `src/**/tests/*.rs` inline test modules)
- path contains a `/benches/` directory segment

All other tracked `.rs` files are **production files**, capped at 500 SLOC.

🟡 **Inline `#[cfg(test)] mod <name> { … }` bodies do not count (#5153).** Add
tests to a 460-SLOC module without splitting it. Only that exact shape is
excluded — `#[cfg(test)] mod tests;` sibling declarations, `#[cfg(test)]` on an
`fn`/`impl`/`use`, and predicates like `all(test, …)` or `any(test, …)` are all
still counted. Braces inside string, byte-string, raw-string, and `'{'` char
literals do NOT skew the region any more; they are blanked before balancing.

🔴 **That region detector is SHARED, and a new consumer inherits its failure
modes.** It lives in `scripts/lib/sloc_awk.sh` and is used by
`scripts/check_line_cap.sh` (to skip test bodies when counting) and
`scripts/check_teardown_guard.sh` (to skip test-only call sites, via
`emit_skip=1`). It is line-based, not a Rust parser, and it fails CLOSED: an
unrecognised spelling leaves the region COUNTED, never silently dropped.

Read that as a per-consumer question before reusing it, because one bias has
two consequences. For the cap, a missed region is a false cap violation —
noise. For the teardown gate, a missed region reported ten test fixtures as
unguarded production writers, and the only way to silence one is a row in
`scripts/teardown-guard-manifest.tsv` — a durable claim that a real write is
exempt, which outlives the mistake and reads later as a considered decision.
A consumer that would fail OPEN on a missed region must not use this detector
as its only check.

🟡 **No standalone SLOC-cap fix.** Never open a PR whose only purpose is bringing
a file back under cap — the split ships inside the PR that next adds to that
file, which is the PR the gate blocks anyway, so it costs one CI cycle instead of
two. That is not licence to leave a red gate red: if your PR trips the cap, split
in that PR. Example — a rebase pushed `bin/tm/main.rs` to 505 SLOC while the
branch was adding a third `RepairAction` arm, and that same PR moved the match
into a submodule.

🔴 Mechanically enforced by `scripts/check_line_cap.sh` in CI and the pre-commit
hook (#610) — a new tracked file over its cap **cannot merge**. Never turn this
gate green by deleting, `#[ignore]`-ing, or excluding a file from the count;
split it instead. Counting definition, the split pattern, when a violation gets
fixed, ratchet-allowlist mechanics, and refactor history:
[docs/reference/sloc-cap.md](docs/reference/sloc-cap.md).

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
must land once, not N times, and silent drift between copies is a hidden risk. Per-domain
consolidation status is in
[docs/reference/domain-consolidation-audit.md](docs/reference/domain-consolidation-audit.md).
Scope: this rule governs capabilities shared ACROSS crates. Duplication of a
capability WITHIN a single crate is not covered by it — consolidate that on
its own merits when the drift has caused (or clearly will cause) a real
defect, not by citing this rule (#4058 binary-name-table consolidation).

The remaining 🟡/🟢 conventions — editions, global state, stderr logging,
dependency declaration, and ignore-tagged tests — are one-liners in the
"Common Pitfalls — Quick Checklist" below, with the reasoning in
[docs/reference/common-pitfalls.md](docs/reference/common-pitfalls.md).

## Git Tag / Release Convention

🔴 **Version bumps, tagging, and publishing are delegated to `local-ops`. The PM
never edits a version file, cuts a tag, or runs `cargo publish` directly.**

Every crate versions and tags independently: `<crate-name>-v<version>`.

Before any bump, tag, or publish, call `Skill(skill="cargo-publish")`. It carries
the full release sequence, the mandatory publish-only-from-merged-main and
identity/clean-tree guards (`scripts/check-publish-ready.sh`,
`scripts/preflight-publish.sh`), cross-crate publish ordering and propagation
waits, the `tga` tag aliases (#1128), and the connection-safe daemon restart.

> **Full release workflow, `scripts/bump-version.sh`, and Developer-ID signing
> setup:** see [docs/reference/release-workflow.md](docs/reference/release-workflow.md).

🔴 **A breaking public-API change needs a matching version bump, and the release
path enforces it (#5050, moved to release-time by #5149).**
`scripts/preflight-publish.sh` CHECK 5 runs `cargo-semver-checks` against the
crate's latest crates.io release immediately before `cargo publish`, and its
nonzero exit is the absolute stop — that is what blocks a bad upload, since
`cargo publish` runs locally and no CI job can stop it. The tag-push workflow
`.github/workflows/semver-checks.yml` reports the same check independently.
Cargo's 0.x rule applies: for a `0.y.z` crate the breaking bump is the MINOR
position. A workspace `cargo check` can never catch this class of break — the
root `Cargo.toml` path override pairs local source with local dependency — which
is how #4088 shipped `trusty-common` 0.22.5's required new public field on a
patch bump and cost `trusty-analyze` 0.7.3 a yank.

🟡 **It does not run on PRs**, so between releases a breaking change can merge
unnoticed and will surface at the release that ships it. Prefer
`#[non_exhaustive]` on public structs and enums so field and variant additions
stay non-breaking by construction, and check a risky change yourself with
`bash scripts/check_semver.sh --crate <crate>`. See
[docs/reference/semver-gate.md](docs/reference/semver-gate.md).

🔴 **CRITICAL macOS note:** never use `cp` to install a release binary on
macOS — always `cargo install`. A plain `cp` over an on-PATH binary leaves a
stale kernel cdhash cache and the next exec is SIGKILL'd as an invalid
signature, which looks exactly like an OOM kill.

🟢 **macOS TCC scope split — read before re-granting anything:** `trusty-search`
(and other external-volume daemons) needs **Full Disk Access**; `trusty-mpm` /
`tm` needs the separate **App Data** category only, and must never be granted
Full Disk Access. Certificate setup, signed-install scripts, the `launchctl
bootout` restart playbook, and orphan-listener verification (#873, #2558, #534,
#2486, #4230) are in the release-workflow reference above.

### Per-PR Changelog Fragment (issue #4476)

🔴 **Every PR that touches a crate's `src/**` adds a changelog FRAGMENT file to
that crate, in the same PR. Never edit a crate's `CHANGELOG.md` by hand.** A PR
that changes crate source and lands with no fragment is a **review-gate
failure** — the same tier as a failing `cargo test` / `cargo clippy` gate — and
also a CI failure (`scripts/check_changelog_fragment.sh`). No "trivial change"
exception. Docs-only, CI-only, test-only and `testdata/` PRs may skip it.

```
crates/<crate>/changelog.d/<issue-or-pr-number>-<short-slug>.md
```

Fragment format and category line: `Skill(skill="tm-workflow")`. Assembler
and CI-gate specifics:
[docs/reference/changelog-fragments.md](docs/reference/changelog-fragments.md).

## Cross-Crate Development Workflow

Cargo resolves internal crates via path automatically, so no `[patch.crates-io]`
dance is needed during development. Modifying a library crate is **rung 4** of
the test ladder above: `cargo check --workspace`, then `cargo test -p <consumer>`
for each direct dependent, all committed together — workspace builds are atomic.
Publish-time `[patch.crates-io]` semantics are in
`Skill(skill="cargo-publish")`.

## Parallel Worktree Discipline

Generic worktree discipline — main checkout inspection-only, provisioning off
`origin/main`, branch-is-the-workstream, one worktree per independently
reviewable PR outcome, subagent confinement, cleanup — lives in
`Skill(skill="tm-workflow")`. It applies here in full. What follows is only what
this repo adds.

**End-to-end delivery chain:** accepted outcome → optional issue → worktree
branch → one cohesive PR → applicable Rust gates → trusty-review gate →
squash-merge → worktree cleanup. `tm-workflow` owns the full sequence and
`tm-ticketing` owns whether the optional issue exists; this file adds only the
Rust-specific gates (see the Rust Test Ladder above).

🟡 **`cargo install` from a worktree, not the main checkout.** The preferred
pattern for installing a freshly-built binary onto your PATH is:

```bash
cargo install --path .claude/worktrees/<dirname>/crates/<name> --locked
```

Cargo writes atomically to a temp file and renames into `~/.cargo/bin/`,
which keeps the macOS kernel's cdhash cache consistent (see the
release-workflow note above). A plain `cp` over an on-PATH binary leaves a stale
cdhash cache and the next exec is SIGKILL'd. The main checkout never needs to be
involved.

> **Extended discipline rationale, the install-from-worktree commands, and the
> stash-first fallback:** see [docs/reference/worktree-discipline.md](docs/reference/worktree-discipline.md).

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
| `taudit` | trusty-audit | `-p trusty-audit` | `crates/trusty-audit/` (bins: `trusty-audit`, `taudit`) |

These abbreviations apply everywhere: ticket descriptions, build commands, references in conversation. Always expand before running `cargo` commands.

> **Auto-resolution:** When connected to trusty-memory MCP, call `get_prompt_context()` at the start of each turn to load current aliases and conventions. Pass a `query` string to filter to relevant facts only.

## Development Environment

### Required Tools

- **Rust**: `rustup` with the toolchain pinned to MSRV `1.94` or later.
  Install: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Node / pnpm**: only needed if working on the Svelte UIs embedded in
  `trusty-search` or `trusty-memory`. Install pnpm via `npm i -g pnpm`.
- **Git**: standard; the workspace uses git tags for per-crate releases.

### Environment Variables

`RUST_LOG` (tracing filter) and `SKIP_UI_BUILD=1` (skip the Svelte UI build in
`build.rs`) are the two you reach for day to day. Every variable, including
`OPENROUTER_API_KEY`, `TRUSTY_LLM_MODEL` and `TRUSTY_NO_KG`, is in
[docs/reference/environment-variables.md](docs/reference/environment-variables.md).

### IDE Setup

> **Full IDE setup reference:** see [docs/reference/ide-setup.md](docs/reference/ide-setup.md).

Quick: VS Code needs `rust-analyzer` + `Even Better TOML` extensions; RustRover auto-detects the workspace.

### Running Individual MCP Servers Locally

> **Detailed MCP server examples and wiring:** see [docs/reference/running-mcp-servers.md](docs/reference/running-mcp-servers.md).

Quick: `RUST_LOG=info cargo run -p trusty-search -- start` (daemon), `cargo run -p trusty-search -- serve` (MCP stdio mode).

## Public Website (`website/`)

SvelteKit + `adapter-vercel`, deployed to Vercel from `main`. Two page
families with DIFFERENT update semantics:

- **`/docs/**`** — generated at build time from files listed in
  `docs/public-manifest.tsv`. Edit a listed `docs/` file, merge to main, and
  the live site updates automatically. A `PAGE` row naming a missing file
  FAILS the build; a `docs/` file absent from the manifest is simply never
  public (an allowlist boundary, not a bug).
- **`/tools/<crate>`** — six hand-authored flagship pages (search, memory,
  mpm, analyze, review, tga). Their copy is static prose in
  `website/src/lib/tools.ts` and `website/src/routes/tools/*/+page.svelte`,
  verified against crate source when written. 🔴 **Editing a crate README does
  NOT update its flagship page** — nothing in the build reads crate READMEs.
  Update those files by hand.

Vercel rebuilds only when a push touches `website/`, `docs/`, `Cargo.lock`, or
`crates/*/Cargo.toml`. A `crates/*/README.md` or root `README.md` change
triggers no rebuild — nor does a `CLAUDE.md` edit, which is expected.

🔴 There is no `vercel.json`. Root Directory, "Include source files outside of
the Root Directory", and the Ignored Build Step are configured in the Vercel
dashboard only — dashboard drift leaves no trace in git. Setup and the full
path table: [website/README.md](website/README.md).

🔴 Website tests do not run in CI (#5200). Run them by hand: `pnpm test` from
INSIDE `website/` — pnpm is pinned there (`packageManager` field); a shell at
the repo root has no such pin and its resolved pnpm can require a Node version
newer than what's installed.

## Common Pitfalls — Quick Checklist

For extended explanations, see [docs/reference/common-pitfalls.md](docs/reference/common-pitfalls.md).

- **Library error handling:** use `thiserror`, not `unwrap()` in libraries
- **Daemon stdout:** never log to stdout in daemons or MCP servers — `init_tracing` writes to stderr so stdout stays clean for MCP JSON-RPC framing
- **Axum in libraries:** gate behind `axum-server` feature flag
- **Shared crate changes:** always run `cargo check` + tests for all dependents
- **SLOC cap:** respect 500/3000 SLOC limits (prod/test); use `bash scripts/check_line_cap.sh`
- **UI build:** install pnpm or set `SKIP_UI_BUILD=1` before `cargo build`
- **Patch tables:** put all `[patch.crates-io]` in root `Cargo.toml` only
- **Workspace deps:** shared external crates are declared once in `[workspace.dependencies]` and referenced as `dep = { workspace = true }` — never pin locally if already in the workspace table
- **Internal deps:** reference sibling crates as `trusty-common = { workspace = true }`; the workspace manifest owns the path, so every member resolves from in-tree source
- **No global state:** helpers are free functions or small structs — no `lazy_static!` / `once_cell::sync::Lazy` except the tracing subscriber, which uses `try_init` to stay idempotent across test binaries
- **MSRV drift:** prefer stable channel toolchains; don't break `rust-version = "1.94"`
- **Edition mismatch:** the workspace *default* is edition 2024 (`edition.workspace = true`); 11 crates pin `edition = "2021"` explicitly. Let-chains (`if let … && let …`) only compile in 2024 — read the crate's `Cargo.toml` before copying one in
- **Ignored tests:** ONNX-backed embedder tests are `#[ignore]`d so CI stays fast; they need `cargo test -- --include-ignored` to run at all

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
- **Domain consolidation audit:** [docs/reference/domain-consolidation-audit.md](docs/reference/domain-consolidation-audit.md) — dated per-domain status behind the common-entry-point rule.
- **Per-PR changelog fragments:** [docs/reference/changelog-fragments.md](docs/reference/changelog-fragments.md) — assembler and CI-gate mechanics.
- **Public-API / SemVer gate:** [docs/reference/semver-gate.md](docs/reference/semver-gate.md) — where it runs (release-time, not per-PR), what it checks, which crates it skips and why, and the feature-exclusion file.
- **Rust test ladder gate commands:** [docs/reference/test-ladder-baseline.md](docs/reference/test-ladder-baseline.md) — the exact command chain per rung, the baseline-failure protocol, and how much gate output a PR body owes.
- **Generated doc regions:** [docs/reference/generated-doc-regions.md](docs/reference/generated-doc-regions.md) — the `<!-- BEGIN GENERATED: … -->` marker contract, the `UPDATE_DOCS=1 cargo test -p <crate> --test generated_docs` regeneration command, and the plainly-stated limit: a crate with no markers is not checked.
- **Rust issue search keys:** [docs/reference/issue-search-keys.md](docs/reference/issue-search-keys.md) — why test name / panic text / symbol / crate find the canonical issue.
- **Public documentation allowlist:** [docs/public-manifest.tsv](docs/public-manifest.tsv) — the curated list of `docs/` pages the public website may publish; format documented in the file header. It is an ALLOWLIST, so a page absent from it is never public. Enforced by `scripts/check_public_docs.sh` (self-test `scripts/check_public_docs_selftest.sh`). The internal mdBook (`docs/book.toml` + `docs/SUMMARY.md`) is unaffected and remains the complete offline book.
