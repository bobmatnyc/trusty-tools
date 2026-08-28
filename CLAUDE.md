# trusty-tools — Claude Code Instructions

Unified Rust workspace for the trusty-* AI tooling ecosystem: 21 crates —
shared libraries, daemon/MCP servers, the MPM platform, the control plane, and
an orchestrator — under one Cargo workspace.

## Project Overview

- Cargo workspace, resolver v2, glob members `crates/*`, MIT License.
- Each crate owns its `version`. `[workspace.package]` shares `rust-version`,
  `edition`, `license`, `repository`, `authors` — no version field (#343).
- **MSRV `1.94`**, enforced in CI with `dtolnay/rust-toolchain@1.94`
  ([ADR-0029](docs/adr/0029-msrv-1-94-and-edition-policy.md)).

## Role & Scope

- `trusty-tools` is the **single source of truth** for all trusty-* AI tooling.
- The authoritative crate list is the `[workspace.members]` glob in the root
  `Cargo.toml` — every `crates/*` subdirectory with a `Cargo.toml` is a member.
- Crate purposes: [crate-map.md](docs/reference/crate-map.md). Replaced repos:
  [former-repos.md](docs/reference/former-repos.md).

## Build and Test Commands

🔴 **Single-path workflows — use exactly these commands.** Scope to the crate
you changed; a bare workspace run is a hardening gate, not an inner loop.

```bash
cargo build                                            # build all crates (dev)
cargo check -p <crate>                                 # fastest — no codegen
cargo test -p <crate> --no-fail-fast                   # test one crate — EVERY target
cargo clippy -p <crate> --all-targets -- -D warnings   # lint one crate
cargo fmt                                              # format (--check to verify only)
```

🔴 **`--no-fail-fast` is not optional (#5354).** Cargo runs each test target as
its own binary and stops issuing further targets the moment one target reports a
failure — it does not run them all and report the aggregate. One failing `--lib`
test therefore hides every integration target behind it, and the run exits
having covered far less than its counts suggest, with nothing at the call site
saying so. It has bitten twice: `--test tm_hook_pm_guard` never executed once
across a full session of runs (#5324), and the
`execute_doctor_against_test_daemon` timing flake masked 49 targets on PR #5904.
Same family as #4307 (a filter matching zero tests reports `ok`) and #4901 (a
non-default feature compiles a module out). Worked example and what a pasted
count then proves: [test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).

- For anything else — release builds, feature-gated tests, `--include-ignored` /
  ONNX tests, a single test by name, the `trusty-search` performance suite,
  `cargo update` / `cargo audit`, running a crate binary — call
  `Skill(skill="cargo-commands")` rather than guessing.
- 🟡 **Crate name ≠ directory name.** `-p <crate>` takes the `name` field from
  the crate's `Cargo.toml`; exceptions are in Abbreviations & Aliases below. On
  "package not found", read the crate's `Cargo.toml`.

🔴 **`trusty-common` takes `--features` on every test run (#4901)** — its
`default` set is empty, so a bare `cargo test -p trusty-common` is a
`compile_error!`. Name what you changed:

```bash
cargo test -p trusty-common --features memory-core,embedder-test-support  # memory_core
cargo test -p trusty-common --features <feature>                          # any other gated module
cargo test -p trusty-common --features unconditional-only                 # only the always-compiled surface
```

- `cargo build` / `cargo check -p trusty-common` are unaffected.
  `--all-features` is unavailable — the `embedder-*` ORT variants are mutually
  exclusive.
- 🟡 Other crates hide the same trap: before trusting a crate-scoped green, check
  whether the module you edited sits behind a non-default feature.

## Rust Test Ladder — how much testing this change needs

🔴 **This ladder is the authoritative answer to "how much testing does this
change need" for this repo.** Run the smallest deterministic gate covering the
change's blast radius. Risk labels map onto the rungs (1–2 Low, 3–4 Normal,
5–6 High).

| # | Change class | Risk | PR gate, in short |
|---|---|---|---|
| 1 | Docs, comments, changelog fragments only | Low | Doc gates only (`check_sld.sh`, plus doc-numbers / line-cap if touched). No Cargo test by default. |
| 2 | Test-only stabilization — flake fix, fixture, test harness | Low | `fmt --check` + `test -p <crate> --no-fail-fast`, with the flake re-run ~10× |
| 3 | Localized behavior inside one crate | Normal | `fmt --check` + `check` + `clippy` + `test --no-fail-fast`, all `-p <crate>`, plus one regression test that provably failed before |
| 4 | **Cross-crate change** — public API or shared library (`trusty-common`, `trusty-embedderd`, …) | Normal → High | Rung 3 on the library, then `check --workspace` + `test -p <consumer> --no-fail-fast` for **each direct dependent** |
| 5 | Cross-crate contract, persistence, security, process lifecycle, **release tooling** | High | Rung 4, plus `--include-ignored` integration coverage, failure-path/concurrency tests, and a `code-critic` round |
| 6 | **UI / API surface** — Svelte UIs, MCP tool schemas, HTTP routes | High | Rung 3 or 4 for the Rust side, plus the UI package's own test/build and one binary smoke run — with direct UI/API evidence, not just crate tests |

- Required tests stay in the implementation PR. Name the rung and paste its
  command in the PR body. Per-rung commands and evidence rules:
  [test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).
- 🔴 **`cargo test --workspace` is not the default inner-loop proof for a
  localized change** — it belongs at the publish boundary; a rung-4 PR does not
  owe one to merge.
- 🔴 **Scope down, never scope away.** A lower rung is a claim about blast radius
  you must be able to prove. Never licence to make a red gate green by deleting,
  `#[ignore]`-ing, `cfg`-gating, `--exclude`-ing, or `--lib`-narrowing coverage.
- **Evidence:** a PR body may summarise a **passing** gate as command + counts +
  scope; raw output stays **mandatory** for failures, flakes, performance claims,
  and disputed results. Counts from a run WITHOUT `--no-fail-fast` prove only the
  targets that ran, so name the flag beside them (#5354).

### What CI actually gates

- 🔴 **Read the required-contexts list live, never hand-copy it** — a stale copy
  cost [#5836](https://github.com/bobmatnyc/trusty-tools/pull/5836) a merge:

  ```bash
  gh api repos/bobmatnyc/trusty-tools/branches/main/protection \
    --jq '.required_status_checks.contexts'
  ```

- Every required job triggers unconditionally (no `paths:` filters) and
  short-circuits on the `docs_only` boolean from the `changes` job
  (`ci.yml:166-195`).
- `required_status_checks.strict` is `false`: a PR's head need not be current
  with `main` for its checks to count.
- 🟡 The required `Clippy` job runs `--workspace --all-targets` but excludes the
  Tauri UI crates (`ci.yml:34-47`); their dedicated per-crate clippy jobs must
  stay *required* to be gates
  ([#5929](https://github.com/bobmatnyc/trusty-tools/pull/5929),
  [#5935](https://github.com/bobmatnyc/trusty-tools/issues/5935)).
- 🔴 **`Rust tests (pre-publish gate)` — the eight shards — does not run on pull
  requests.** Not required, skipped outright on a PR; runs on push to `main` and
  `workflow_dispatch`. Do not wait for it.
- 🔴 **Those shards run `cargo nextest`, which gives every test its own PROCESS,
  so `#[serial_test::serial]` serializes nothing there (#4162).** The `$HOME`
  isolation PR #4120 established survives — a `set_var("HOME")` is process-local,
  so nextest's isolation is strictly stronger than an in-process lock for env
  state. What lapses is any `#[serial]` guarding a resource shared ACROSS
  processes (a fixed path, a fixed port, one real file): use
  `#[serial_test::file_serial]` for those, and keep redirecting `$HOME` per test.
  Measurements: [test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).
- 🟡 A PR proves every test target COMPILES; test EXECUTION defers to `main`, so
  run the ladder rung your change earns before merging. Full suite on a branch:
  Actions → CI → "Run workflow".
- 🟡 **A red `main` files an issue** — `notify-main-failure` opens or comments on
  the `ci-red-main`-labelled tracking issue, then fails the run.
- 🟡 **A `BEHIND` branch merges fine** — use `gh pr merge --squash
  --delete-branch --auto`
  ([#5958](https://github.com/bobmatnyc/trusty-tools/pull/5958)). What blocks is
  `mergeStateStatus` reporting `BLOCKED` (pending or failing checks); updating
  for BEHIND alone restarts CI and can fail to converge. `gh pr update-branch
  <n>` stays correct for a genuine `CONFLICTING` state.
- 🔴 **A PR predating a newly-required job must `update-branch`** — its branch
  lacks the commit that ADDED the job, so it can never produce that check run and
  sits `BLOCKED` with nothing red and the context absent from `gh pr checks`
  ([#5962](https://github.com/bobmatnyc/trusty-tools/pull/5962)). Adding a
  required context wedges every open PR that predates it.
- 🟡 **Do not use `gh pr merge --admin`** — the account is repo owner and the
  flag is not a no-op for `BLOCKED` or `BEHIND`. Every required context passing
  on the PR's own head remains the bar.

### Baseline failures — the Rust specifics

<!-- Load-bearing order below: keep the if/otherwise together — see the comment in test-ladder-baseline.md for why. -->
🔴 **Never turn a red gate green by `#[ignore]`-ing, `cfg`-gating, or
`--exclude`-ing a failing test — prove the failure is pre-existing instead.**
If the failing crate depends on nothing you changed, prove it with an empty
`git diff --name-only origin/main...HEAD -- <crate>/`; otherwise that diff
proves nothing and you must reproduce the failure on `origin/main` instead.

- Known-environmental flaky tests, the five-step pre-existing-red protocol, and
  the report-string format:
  [test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).

## Key Conventions

🔴 **Search before filing.** `tm-ticketing` owns whether a finding earns an issue
and the disposition on a hit (`COMMENT` / `REOPEN` / `NEW REGRESSION` /
`NO TICKET` — not automatically an append, #5202). Search open and recently
closed issues by **test name**, **panic / error text**, **affected symbol**, and
**crate** ([issue-search-keys.md](docs/reference/issue-search-keys.md)).

🔴 **Issue lifecycle — open → coded → merged → tested → closed.** Three mutually
exclusive labels between GitHub's native open/closed:

| Label | Meaning |
|---|---|
| `status:coded` | Implementation pushed on a branch; PR not yet merged |
| `status:merged` | PR merged to main; live verification pending |
| `status:tested` | Verified live (installed binary / real run); eligible to close |

- Advancing a state removes the prior label in the same edit:
  `gh issue edit N --add-label status:merged --remove-label status:coded`.
- Fix PRs use `Refs #N`, **never** `Closes #N` — merge must not auto-close.
- An issue closes only from `status:tested`, with live verification evidence in
  the closing comment. A merged fix that fails live verification stays open.

🔴 **Why/What/Test doc pattern with proportional depth:**

```rust
/// Why: <motivation>   /// What: <mechanics>   /// Test: <where coverage lives>
```

- Mandatory in full for API entry points, design-heavy code, error contracts,
  safety/TCC behavior, and cross-crate surfaces. One line suffices for trivial
  items (simple getters, obvious one-liners, thin re-exports).
- Defensive-reasoning paragraphs and issue-history anecdotes go in linked ADRs or
  issues, not inline comments — use `// See <issue-or-adr>`. Worked examples:
  `Skill(skill="documentation-style")`.

🟡 **Ticket-attributed inline comments** — leave `// #1234: <one-line reason>` or
`// See #1234` at the change site. One line, never a narrative.

🔴 **No `unwrap()` in library code** — `?` with `anyhow::Result` for
application/binary code, `thiserror` for library error types. Reserve `expect()`
for invariants that can never occur at runtime.

🔴 **`thiserror` for libraries, `anyhow` for binaries** — library crates define
structured error enums with `#[derive(thiserror::Error)]`; binary and daemon
crates use `anyhow::Result`.

🔴 **Feature flags** — `trusty-common` gates `axum` and `tower-http` behind the
`axum-server` feature. Never add axum as an unconditional dependency in a library
crate; enable it explicitly in crates that serve HTTP.

🔴 **SLOC file size hard cap (MECHANICALLY ENFORCED, dual-cap since #1131,
TEST_CAP raised #4074):**

| File type | SLOC cap |
|---|---|
| Production source files | **500 SLOC** |
| Test / benchmark files | **3000 SLOC** |

- Comments, doc comments, and blank lines do **not** count — only non-comment
  code lines in tracked `.rs` files ([sloc-cap.md](docs/reference/sloc-cap.md)).
- A file is a **test/benchmark file** when ANY match: basename exactly
  `tests.rs`; basename ending `_test.rs` or `_tests.rs`; a `/tests/` path segment
  (covers `crates/*/tests/*.rs` and `src/**/tests/*.rs`); a `/benches/` path
  segment. All other tracked `.rs` files are **production**, capped at 500.
- 🟡 **Inline `#[cfg(test)] mod <name> { … }` bodies do not count (#5153)** —
  only that exact shape. `#[cfg(test)] mod tests;` sibling declarations,
  `#[cfg(test)]` on an `fn`/`impl`/`use`, and `all(test, …)` / `any(test, …)`
  predicates are all still counted.
- 🔴 Enforced by `scripts/check_line_cap.sh` in CI and the pre-commit hook
  (#610) — a new tracked file over its cap **cannot merge**. Never green this
  gate by deleting, `#[ignore]`-ing, or excluding a file from the count; split it.
- 🟡 **No standalone SLOC-cap fix** — the split ships inside the PR that next
  adds to that file. Not licence to leave a red gate red: if your PR trips the
  cap, split in that PR.

🔴 **The SLOC region detector is SHARED, and a new consumer inherits its failure
modes.** `scripts/lib/sloc_awk.sh` serves `check_line_cap.sh` (skip test bodies
when counting) and `check_teardown_guard.sh` (skip test-only call sites, via
`emit_skip=1`). It is line-based, not a Rust parser, and fails CLOSED: an
unrecognised spelling leaves the region COUNTED.

- Weigh that bias per consumer before reusing it — a false cap violation is
  noise, but the teardown gate's only silencer is a durable row in
  `scripts/teardown-guard-manifest.tsv`.
- A consumer that would fail OPEN on a missed region must not use this detector
  as its only check.

🔴 **Common entry point, clean domain demarcation** — every capability shared
across two or more crates (spawning git/gh/tmux/launchctl, building an HTTP
client, resolving a daemon's address, reading a secret or config value, redacting
output, retrying a fallible call) MUST have exactly one implementation, in
`trusty-common` or the crate owning that domain, that every consumer routes
through. A second independent implementation is a defect.

- Before writing `Command::new(...)`, `reqwest::Client::builder()`,
  `std::env::var(...)` for a cross-crate concern, or bespoke read-this-config /
  find-this-daemon / scrub-this-string logic: search first (`git grep`, then the
  trusty-common source tree) and extend rather than duplicate.
- Scope: capabilities shared ACROSS crates. Duplication WITHIN one crate is not
  covered — consolidate that on its own merits (#4058). Per-domain status:
  [domain-consolidation-audit.md](docs/reference/domain-consolidation-audit.md).

Remaining 🟡/🟢 conventions — editions, global state, stderr logging, dependency
declaration, ignore-tagged tests — are one-liners in "Common Pitfalls" below.

## Git Tag / Release Convention

🔴 **Version bumps, tagging, and publishing are delegated to `local-ops`. The PM
never edits a version file, cuts a tag, or runs `cargo publish` directly.**

- Every crate versions and tags independently: `<crate-name>-v<version>`.
- Before any bump, tag, or publish, call `Skill(skill="cargo-publish")` — it
  carries the release sequence, the publish-only-from-merged-main and
  identity/clean-tree guards (`check-publish-ready.sh`, `preflight-publish.sh`),
  cross-crate ordering, the `tga` tag aliases (#1128), and the connection-safe
  daemon restart. Full workflow and Developer-ID signing:
  [release-workflow.md](docs/reference/release-workflow.md).

🔴 **Internal consistency is the bar — do not deliberate over external SemVer.**
Keep the workspace self-consistent. What a third-party crates.io consumer would
experience is not a question to weigh, hold work over, or write an analysis about.

🔴 **That governs deliberation, not the gate (#5050, release-time since #5149).**
`preflight-publish.sh` CHECK 5 runs `cargo-semver-checks` immediately before
`cargo publish`, and its nonzero exit is the absolute stop — no CI job can stop a
bad local upload. Never treat it as advisory or silence it with an
exclusions-file row.

🔴 **A zero exit is NOT the mirror of that stop (#5620)** — `check_semver.sh`
exits 0 both when it compared a crate cleanly and when it compared nothing.

- **`0 compared` and `[PASS]` are unreachable together.** A recorded skip prints
  `[SKIP]` and permits; a blind gate prints `[FAIL]` and stops.
- `PREFLIGHT_SEMVER_UNVERIFIED="<reason>"` downgrades a blind gate to `[WARN]` —
  a reason string, never a boolean, and never for a standing machine limitation
  (that belongs in `scripts/semver-checks-feature-exclusions.tsv`).
- Cargo's 0.x rule applies: for a `0.y.z` crate the breaking bump is MINOR.
- A workspace `cargo check` never catches this class of break — the root
  `Cargo.toml` path override pairs local source with local dependency (#4088).

🟡 **On an ordinary PR the semver gate compares nothing** — `semver-checks.yml`
exits in ~15s when no crate version is bumped (#5311). Expected, not something to
pre-empt; `#[non_exhaustive]` on public structs and enums keeps it quiet at
release time. Check a change yourself: `bash scripts/check_semver.sh --crate
<crate>` ([semver-gate.md](docs/reference/semver-gate.md)).

🔴 **The tag must name the commit that gets published** — `preflight-publish.sh`
CHECK 6 (`check-tag-publish-parity.sh`) binds them, because the earlier guards
accept a tag behind HEAD. **Fast-forwarded after tagging? Reset the checkout back
to the tagged commit and publish that** — `git reset --hard <tag>`. After
`cargo publish`, run `make publish-verify CRATE=<crate>`
([parity guard](docs/reference/release-workflow.md#tagpublish-commit-parity-guard)).

🔴 **Release tags here are immutable — a stranded tag burns its version number
(#6178).** A ruleset rejects force-update and delete of a `*-v*` tag with GH013;
`admin: true` does not lift it and the ruleset is invisible to the API.

- Re-tagging, moving a tag, or delete-and-re-push cannot execute here — reset the
  checkout to the tag instead.
- When the tagged commit can never pass the publish gate, that version number is
  spent: **bump to the next version and tag fresh.**
- Tag as late as possible, immediately before `cargo publish`.

🔴 **CRITICAL macOS note:** never use `cp` to install a release binary on macOS —
always `cargo install`. A `cp` over an on-PATH binary leaves a stale kernel
cdhash cache and the next exec is SIGKILL'd as an invalid signature, which looks
exactly like an OOM kill.

🟢 **macOS TCC scope split — read before re-granting anything:** `trusty-search`
(and other external-volume daemons) needs **Full Disk Access**; `trusty-mpm` /
`tm` needs the separate **App Data** category only, and must never be granted
Full Disk Access. Certificates, signed-install scripts, the `launchctl bootout`
restart playbook, and orphan-listener verification (#873, #2558, #534, #2486,
#4230): [release-workflow.md](docs/reference/release-workflow.md).

### Per-PR Changelog Fragment (issue #4476)

🔴 **Every PR that touches a crate's `src/**` adds a changelog FRAGMENT file to
that crate, in the same PR. Never edit a crate's `CHANGELOG.md` by hand.** A PR
that changes crate source and lands with no fragment is a **review-gate
failure** — the tier of a failing `cargo test` / `cargo clippy` gate — and a CI
failure (`scripts/check_changelog_fragment.sh`). No "trivial change" exception.
Docs-only, CI-only, test-only and `testdata/` PRs may skip it.

```
crates/<crate>/changelog.d/<issue-or-pr-number>-<short-slug>.md
```

- Format and category line: `Skill(skill="tm-workflow")`. Assembler and CI-gate
  specifics: [changelog-fragments.md](docs/reference/changelog-fragments.md).

## Cross-Crate Development Workflow

- Cargo resolves internal crates via path automatically — no `[patch.crates-io]`
  dance during development.
- Modifying a library crate is **rung 4**: `cargo check --workspace`, then
  `cargo test -p <consumer>` for each direct dependent, all committed together.
- Publish-time `[patch.crates-io]` semantics: `Skill(skill="cargo-publish")`.

## Parallel Worktree Discipline

Generic worktree discipline — main checkout read-only for source (mechanically
enforced;
[ADR-0044](docs/adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md),
[ADR-0048](docs/adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md)),
provisioning off `origin/main`, branch-is-the-workstream, one worktree per
independently reviewable PR outcome, subagent confinement, cleanup — lives in
`Skill(skill="tm-workflow")` and applies here in full. This repo adds only:

- **Delivery chain:** accepted outcome → optional issue → worktree branch → one
  cohesive PR → applicable Rust gates → trusty-review gate → squash-merge →
  worktree cleanup. This file adds only the Rust gates (the test ladder above).
- 🟡 **`cargo install` a clean checkout, never `cp`** — Cargo renames atomically
  into `~/.cargo/bin/`, keeping the macOS cdhash cache consistent:

  ```bash
  cargo install --path .claude/worktrees/<dirname>/crates/<name> --locked
  ```

- `--path` bakes in whatever is on disk, uncommitted edits included. Install only
  from a checkout with an empty `git status --porcelain` at a known commit —
  check it, don't assume it. A fresh worktree off `origin/main` satisfies that by
  construction and stays the default.
- The main checkout is not automatically disqualified: the write boundary
  restricts SOURCE writes only, so docs and configuration (`.md` included) stay
  writable and committable there
  ([ADR-0049](docs/adr/0049-docs-commits-are-permitted-in-a-main-checkout.md)).
- Extended rationale and the throwaway-worktree fallback for a dirty checkout:
  [worktree-discipline.md](docs/reference/worktree-discipline.md).

## Abbreviations & Aliases

Resolve any crate abbreviation with this table before taking action — it applies
everywhere: ticket descriptions, build commands, conversation.

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

> **Auto-resolution:** When connected to trusty-memory MCP, call
> `get_prompt_context()` at the start of each turn to load current aliases and
> conventions. Pass a `query` string to filter to relevant facts only.

## Development Environment

- **Rust**: `rustup`, toolchain at MSRV `1.94` or later
  (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`).
- **Node / pnpm**: only for the Svelte UIs in `trusty-search` / `trusty-memory`
  (`npm i -g pnpm`).
- **Env vars**: `RUST_LOG` and `SKIP_UI_BUILD=1` (skip the Svelte UI build in
  `build.rs`) are the day-to-day two. Full table:
  [environment-variables.md](docs/reference/environment-variables.md).
- **IDE**: VS Code needs `rust-analyzer` + `Even Better TOML`; RustRover
  auto-detects ([ide-setup.md](docs/reference/ide-setup.md)).
- **MCP servers locally**: `RUST_LOG=info cargo run -p trusty-search -- start`
  (daemon), `cargo run -p trusty-search -- serve` (MCP stdio). Wiring:
  [running-mcp-servers.md](docs/reference/running-mcp-servers.md).

## Public Website (`website/`)

SvelteKit + `adapter-vercel`, deployed to Vercel from `main`. Two page families
with DIFFERENT update semantics:

- **`/docs/**`** — generated at build time from files listed in
  `docs/public-manifest.tsv`. Edit a listed `docs/` file, merge to main, and the
  live site updates. A `PAGE` row naming a missing file FAILS the build; a
  `docs/` file absent from the manifest is never public (allowlist, not a bug).
- **`/tools/<crate>`** — six hand-authored flagship pages (search, memory, mpm,
  analyze, review, tga), static prose in `website/src/lib/tools.ts` and
  `website/src/routes/tools/*/+page.svelte`.
- 🔴 **Editing a crate README does NOT update its flagship page** — nothing in
  the build reads crate READMEs. Update those files by hand.
- Vercel rebuilds only when a push touches `website/`, `docs/`, `Cargo.lock`, or
  `crates/*/Cargo.toml` — not a `README.md` or `CLAUDE.md` change.
- 🔴 There is no `vercel.json`. Root Directory, "Include source files outside of
  the Root Directory", and the Ignored Build Step live in the Vercel dashboard
  only — dashboard drift leaves no trace in git
  ([website/README.md](website/README.md)).
- 🟡 Website tests run under `.github/workflows/website-tests.yml` (#5200), not
  `ci.yml`'s `ui-checks`. Run them by hand before pushing: `pnpm test` from
  INSIDE `website/`, where pnpm is pinned (`packageManager`). The workflow runs
  Node 20 — a newer local Node reports spurious `localStorage` failures in
  `theme.test.ts`.

## Common Pitfalls — Quick Checklist

Rules already stated above (error handling, axum gating, SLOC caps, dependent
testing) are not repeated here. Extended explanations:
[common-pitfalls.md](docs/reference/common-pitfalls.md).

- **Daemon stdout:** never log to stdout in daemons or MCP servers — `init_tracing` writes to stderr so stdout stays clean for MCP JSON-RPC framing
- **Line-cap check:** `bash scripts/check_line_cap.sh`
- **UI build:** install pnpm or set `SKIP_UI_BUILD=1` before `cargo build`
- **Patch tables:** put all `[patch.crates-io]` in root `Cargo.toml` only
- **Workspace deps:** shared external crates are declared once in `[workspace.dependencies]` and referenced as `dep = { workspace = true }` — never pin locally if already in the workspace table; `default-features` is likewise owned by the root entry, so `default-features = false` on a member is ignored unless the root entry sets it too
- **Internal deps:** reference sibling crates as `trusty-common = { workspace = true }`; the workspace manifest owns the path
- **No global state:** helpers are free functions or small structs — no `lazy_static!` / `once_cell::sync::Lazy` except the tracing subscriber, which uses `try_init` to stay idempotent across test binaries
- **MSRV drift:** prefer stable channel toolchains; don't break `rust-version = "1.94"`
- **Edition mismatch:** the workspace *default* is edition 2024 (`edition.workspace = true`); 11 crates pin `edition = "2021"` explicitly. Let-chains (`if let … && let …`) only compile in 2024 — read the crate's `Cargo.toml` before copying one in
- **Ignored tests:** ONNX-backed embedder tests are `#[ignore]`d so CI stays fast; they need `cargo test -- --include-ignored` to run at all

## Reference Documentation

Most references are linked from the rule they serve, above. Not linked elsewhere:

- [documentation-layout.md](docs/reference/documentation-layout.md) — docs layout conventions
- [DOC-38](docs/specs/spec-linked-documentation.md) — SLD policy, enforced by `scripts/check_sld.sh`
- [threat-model.md](docs/reference/threat-model.md) — per-daemon bind/guard/proxy inventory ([ADR-0018](docs/adr/0018-loopback-only-doctrine.md))
- [generated-doc-regions.md](docs/reference/generated-doc-regions.md) — the `<!-- BEGIN GENERATED: … -->` contract and `UPDATE_DOCS=1 cargo test -p <crate> --test generated_docs`; a crate with no markers is not checked
- [public-manifest.tsv](docs/public-manifest.tsv) — ALLOWLIST of publishable `docs/` pages (absent = never public), enforced by `scripts/check_public_docs.sh`; the internal mdBook is unaffected
