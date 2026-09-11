# trusty-tools — Claude Code Instructions

Unified Rust workspace for the trusty-* AI tooling ecosystem: shared libraries,
daemon/MCP servers, orchestration harnesses, control-plane tools, and desktop
clients under one Cargo workspace. Derive the live package count from
`cargo metadata --no-deps --format-version 1`; do not hand-copy it here.

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

🔴 **`--no-fail-fast` is not optional (#5354).** Cargo stops issuing further test
targets the moment one target reports a failure, so a single failing `--lib` test
hides every integration target behind it and the run exits having covered far
less than its counts suggest. Worked example, the two incidents it caused, and
what a pasted count then proves:
[test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).

- For anything else — release builds, feature-gated tests, `--include-ignored` /
  ONNX tests, a single test by name, the `trusty-search` performance suite,
  `cargo update` / `cargo audit`, running a crate binary — call
  `Skill(skill="cargo-commands")` rather than guessing.
- 🟡 **Crate name ≠ directory name.** `-p <crate>` takes the `name` field from
  the crate's `Cargo.toml`; exceptions are in Abbreviations & Aliases below. On
  "package not found", read the crate's `Cargo.toml`.
- 🟡 **Editing `crates/trusty-mpm/src/assets/instructions/sections/*.md` needs a
  golden refresh first (#6937).** Three snapshot tests fail before any real gate
  runs otherwise. Run `UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden`, then
  read the diff of the three `crates/trusty-mpm/src/core/testdata/pm-prompt-*.md`
  goldens and confirm it carries only your edit.
- 🟡 An exit 137 with no output from `cargo test -p <crate>` is a SIGKILL under
  memory pressure (several agent worktrees building at once), not a test
  failure; re-run with `-- --test-threads=4`.

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

🔴 **Run the smallest deterministic gate covering the change's blast radius.**
The six rungs, their change classes and risk labels, the exact command each rung
owes, and the stage model are
[test-ladder-baseline.md](docs/reference/test-ladder-baseline.md). Pick the rung
there, name it in the PR body, and paste the command you ran. Required tests
stay in the implementation PR.

🔴 **Scope down, never scope away.** A lower rung is a claim about blast radius
you must be able to prove — never licence to make a red gate green by deleting,
`#[ignore]`-ing, `cfg`-gating, `--exclude`-ing, or `--lib`-narrowing coverage.
`cargo test --workspace` is a publish-boundary gate, not the inner-loop proof for
a localized change.

🟡 **Evidence:** a PR body may summarise a **passing** gate as command + counts +
scope; raw output stays **mandatory** for failures, flakes, performance claims,
and disputed results. Counts from a run WITHOUT `--no-fail-fast` prove only the
targets that ran, so name the flag beside them (#5354).

🔴 **A red gate is triaged, never silenced** — prove the failure is pre-existing
instead. Known-environmental flaky tests, the five-step crate-scoped
confirmation, and the report-string format:
[test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).

🟡 **What CI actually gates** — the live required-contexts read, the eight-shard
pre-publish gate that never runs on a PR, `BEHIND` vs `BLOCKED`, why `--admin` is
not a workaround, and the two red jobs that block nothing:
[ci-gates.md](docs/reference/ci-gates.md). Never hand-copy the required-contexts
list.


## Key Conventions

🔴 **Search before filing.** `tm-ticketing` owns whether a finding earns an issue
and the disposition on a hit (`COMMENT` / `REOPEN` / `NEW REGRESSION` /
`NO TICKET` — not automatically an append, #5202). Search open and recently
closed issues by **test name**, **panic / error text**, **affected symbol**, and
**crate** ([issue-search-keys.md](docs/reference/issue-search-keys.md)).

🔴 **Issue lifecycle — open → in-progress → coded → merged → tested → closed.**
Four mutually exclusive labels between GitHub's native open/closed:

| Label | Meaning |
|---|---|
| `status:in-progress` | A session/agent has claimed it and is actively working it |
| `status:coded` | Implementation pushed on a branch; PR not yet merged |
| `status:merged` | PR merged to main; live verification pending |
| `status:tested` | Verified live (installed binary / real run); eligible to close |

- `status:in-progress` goes on at dispatch, with a comment naming the claiming
  session and the date. Another session takes a claimed issue ONLY when the
  claim is provably stale: the named session is gone AND nothing referencing
  the issue (branch push, PR, comment) has moved since the claim. When in
  doubt, leave it.
- Advance with `tm issue transition N status:merged`, run from the repo root — it
  reads [`issue-state.yaml`](issue-state.yaml), refuses an undeclared edge, and
  makes the add and remove one `gh issue edit`. `tm issue states | current N |
  repair N` round out the verbs. Hand-edit labels only on a host with no `tm`.
- Fix PRs use `Refs #N`, **never** `Closes #N` — merge must not auto-close.
- An issue closes only from `status:tested`, with live verification evidence in
  the closing comment: `tm issue transition N closed --note "<evidence>"`, which
  refuses to run without the note. A merged fix that fails live verification
  stays open.
- A merged fix that fails live verification and draws a follow-up fix PR goes
  back to `status:coded` — `tm issue transition N status:coded` (owner ruling
  2026-09-07). The issue is being coded again, so `status:merged` is stale.

🔴 **`.trusty-mpm/sessions/` is TRACKED** (owner ruling 2026-08-31) — session
snapshots and the pause log are committed so other sessions and harnesses can
read prior work and intent. Commit new snapshot files after each pause. The
re-include at the end of `.gitignore` overrides the auto-managed ignore block;
do not "fix" either rule.

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

🔴 **No `unwrap()` in library code, and `thiserror` for libraries / `anyhow` for
binaries** — library crates define structured error enums with
`#[derive(thiserror::Error)]`; binary and daemon crates use `anyhow::Result` with
`?`. Reserve `expect()` for invariants that can never occur at runtime.

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
- A file is a **test/benchmark file** when its basename or a path segment says so
  (`tests.rs`, `_test.rs`/`_tests.rs`, `/tests/`, `/benches/`); everything else
  tracked is production. Inline `#[cfg(test)] mod <name> { … }` bodies do not
  count (#5153). Exact rules: [sloc-cap.md](docs/reference/sloc-cap.md).
- 🔴 Enforced by `scripts/check_line_cap.sh` in CI and the pre-commit hook
  (#610) — a new tracked file over its cap **cannot merge**. Never green this
  gate by deleting, `#[ignore]`-ing, or excluding a file from the count; split it.
- 🟡 **No standalone SLOC-cap fix** — the split ships inside the PR that next
  adds to that file. Not licence to leave a red gate red: if your PR trips the
  cap, split in that PR.

🟡 **The SLOC region detector (`scripts/lib/sloc_awk.sh`) is SHARED, and a new
consumer inherits its failure modes** — it is line-based, fails CLOSED, and that
bias suits one consumer better than the other:
[sloc-cap.md](docs/reference/sloc-cap.md).

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
That governs deliberation, never the gate: `preflight-publish.sh` CHECK 5 is the
absolute stop, and `check_semver.sh` exiting 0 is not its mirror — `0 compared`
and `[PASS]` are unreachable together (#5050, #5149, #5620). The gate's skip
model, the `PREFLIGHT_SEMVER_UNVERIFIED` reason string, the 0.x bump rule, and
the `[workspace.dependencies]` widening a `0.y` MINOR owes:
[semver-gate.md](docs/reference/semver-gate.md).

🔴 **The tag must name the commit that gets published, and a pushed tag is
immutable — a stranded tag burns its version number.** Fast-forwarded after
tagging? Reset the checkout to the tag, never the tag to the checkout. Tag as
late as possible, immediately before `cargo publish`. Parity guard (CHECK 6),
the GH013 ruleset, and the per-situation remedy table:
[release-workflow.md](docs/reference/release-workflow.md).

🔴 **CRITICAL macOS note:** never use `cp` to install a release binary on macOS —
always `cargo install`. A `cp` over an on-PATH binary leaves a stale kernel
cdhash cache and the next exec is SIGKILL'd as an invalid signature, which looks
exactly like an OOM kill.

🟢 **macOS TCC scope split — read before re-granting anything:** `trusty-search`
(and other external-volume daemons) needs **Full Disk Access**; `trusty-mpm` /
`tm` needs the separate **App Data** category only, and must never be granted
Full Disk Access. Certificates, signed-install scripts, the `launchctl bootout`
restart playbook, which binaries need no signing at all, and orphan-listener
verification (#873, #2558, #534, #2486, #4230, #4750):
[release-workflow.md](docs/reference/release-workflow.md).

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

- 🔴 **One category per fragment (#7287)** — the first line IS the category and
  everything after it belongs to it; two categories mean two files. Validate
  before committing: `bash scripts/check_changelog_fragment.sh --file <path>`.
- 🟡 The exemption is decided by FILE PATH, never by what changed inside the file
  (#7033), and the default gate run needs a real commit rather than the working
  tree (#6947). Format, category list, path classification, the assembler and
  the CI gate: [changelog-fragments.md](docs/reference/changelog-fragments.md)
  and `Skill(skill="tm-workflow")`.

## Cross-Crate Development Workflow

- Cargo resolves internal crates via path automatically — no `[patch.crates-io]`
  dance during development; publish-time semantics: `Skill(skill="cargo-publish")`.
- Modifying a library crate is **rung 4**: `cargo check --workspace`, then
  `cargo test -p <consumer>` for each direct dependent, all committed together.

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
  When staging worktree changes with git, always name files explicitly: `git add <file>` or `git add -p`.
  Never use `git add -A` in a worktree, as it stages untracked build directories like `target-worktree/`.
  Verify the ignored name with `git check-ignore -v target-worktree/` from the worktree root.
- The main checkout is not automatically disqualified: the write boundary
  restricts SOURCE writes only, so docs and configuration (`.md` included) stay
  writable and committable there
  ([ADR-0049](docs/adr/0049-docs-commits-are-permitted-in-a-main-checkout.md)).
- Extended rationale and the throwaway-worktree fallback for a dirty checkout:
  [worktree-discipline.md](docs/reference/worktree-discipline.md).
- 🔴 **The isolation-worktree refusals come from the Claude Code harness, not
  from `tm hook --pm-guard`** ([#6982](https://github.com/bobmatnyc/trusty-tools/issues/6982),
  open upstream) — **do not route another fix for them here.** Take the
  substitute instead:

  | Refused inside a worktree | Use instead |
  |---|---|
  | `git diff` in any argument shape, bare or from the worktree's own cwd | `git --no-pager diff …`, or `git -C <absolute worktree path> diff …` |
  | a diff of one path at one commit | `git show <sha> -- <path>` |
  | a git command inside a redirect-then-`echo` gate chain, or after `cd` | run it bare, one git command per Bash call |
  | `bash scripts/<name>.sh` | `./scripts/<name>.sh` |
  | a heredoc, a `for` loop, `awk -f` / `sed -f` | write the script with the Write tool, then run that file |
  | `$PWD`, `$(…)` or a variable inside a path or an env assignment | spell the absolute path literally |
  | an argument whose text merely contains `git` | re-spell the pattern, or quote a glob |

  Every reported shape, its wording and its substitute:
  [worktree-discipline.md](docs/reference/worktree-discipline.md).

## Abbreviations & Aliases

🔴 **Resolve any crate abbreviation before acting on it** — `tga`, `tm`, `ts`,
`tc`, `ta`, `mpm`, `tagent`, `tcode`, `tctl`, `taudit` all name a crate whose
`-p` flag or directory differs from the abbreviation. The full table:
[crate-aliases.md](docs/reference/crate-aliases.md).

## Development Environment

- **Rust**: `rustup`, toolchain at MSRV `1.94` or later.
- **Node / pnpm**: only for the Svelte UIs, which all live in
  `crates/trusty-console` since #6155 — its own `ui/`, plus `ui-search/`,
  `ui-memory/` and `ui-analyze/` (`npm i -g pnpm`).
- **Env vars**: `RUST_LOG` and `SKIP_UI_BUILD=1` (skip the Svelte UI build in
  `build.rs`) are the day-to-day two. Full table:
  [environment-variables.md](docs/reference/environment-variables.md).
- **IDE**: VS Code needs `rust-analyzer` + `Even Better TOML`; RustRover
  auto-detects ([ide-setup.md](docs/reference/ide-setup.md)).
- **MCP servers locally**: `RUST_LOG=info cargo run -p trusty-search -- start`
  (daemon), `cargo run -p trusty-search -- serve` (MCP stdio). Wiring:
  [running-mcp-servers.md](docs/reference/running-mcp-servers.md).

## Public Website (`website/`)

SvelteKit + `adapter-vercel`, deployed to Vercel from `main`.

- 🔴 **Editing a crate README does NOT update its flagship page** — nothing in
  the build reads crate READMEs. Edit `website/src/content/tools/<slug>.md`.
- 🔴 **`docs/public-manifest.tsv` is an allowlist** — a `docs/` file absent from
  it is never public; a `PAGE` row naming a missing file fails the build.
- Which file to edit for which page, the `<!-- include: -->` directive, the
  Vercel rebuild triggers, the dashboard-only settings (there is no
  `vercel.json`), and running `pnpm test` from inside `website/`:
  [website/README.md](website/README.md).

## Common Pitfalls — Quick Checklist

Rules already stated above (error handling, axum gating, SLOC caps, dependent
testing) are not repeated here. Extended explanations:
[common-pitfalls.md](docs/reference/common-pitfalls.md).

- **Daemon stdout:** never log to stdout in daemons or MCP servers — `init_tracing` writes to stderr so stdout stays clean for MCP JSON-RPC framing
- **Line-cap check:** `./scripts/check_line_cap.sh` — every gate in this file is
  written `bash scripts/…`; inside an isolation worktree that form is sometimes
  refused and `./scripts/…` always runs (#6982)
- **UI build:** install pnpm or set `SKIP_UI_BUILD=1` before `cargo build`
- **Patch tables:** put all `[patch.crates-io]` in root `Cargo.toml` only
- **Workspace deps:** declare shared externals once in `[workspace.dependencies]` and reference them as `dep = { workspace = true }` — never pin locally, and `default-features` is owned by the root entry too, so a member's `default-features = false` is ignored unless the root sets it
- **Internal deps:** reference sibling crates as `trusty-common = { workspace = true }`; the workspace manifest owns the path
- **No global state:** helpers are free functions or small structs — no `lazy_static!` / `once_cell::sync::Lazy` except the tracing subscriber, which uses `try_init` to stay idempotent across test binaries
- **No process-global env in the `tm` bin target (#5544):** `std::env::set_var`/`remove_var` under `crates/trusty-mpm/src/bin/tm/**` is ratcheted to 0 for new files — inject the value instead
- **MSRV drift:** prefer stable channel toolchains; don't break `rust-version = "1.94"`
- **Edition mismatch:** the workspace default is edition 2024; some crates pin `edition = "2021"`. Let-chains (`if let … && let …`) only compile in 2024 — read the crate's `Cargo.toml` before copying one in
- **Ignored tests:** ONNX-backed embedder tests are `#[ignore]`d; they need `cargo test -- --include-ignored` to run at all

## UI Design System (Foundry)

🟡 **All trusty-* UI builds to Foundry**, the Trusty-suite design system at
[docs/design/UI/](docs/design/UI/README.md) — NOT inside any crate. Reconcile new
UI to its tokens, components and screen reference before inventing layout. Spec:
[DOC-39](docs/specs/trusty-code-harness-ui.md) §8 (#3153).

## Reference Documentation

Most references are linked from the rule they serve, above. Not linked elsewhere:

- [ci-scripts.md](docs/reference/ci-scripts.md) — the `scripts/` checks that run only in a workflow, and which of them block a merge
- [ci-gates.md](docs/reference/ci-gates.md) — required contexts, merge states, and the jobs that gate nothing
- [test-ladder-baseline.md](docs/reference/test-ladder-baseline.md) — the six rungs, their commands, and baseline-red triage
- [crate-aliases.md](docs/reference/crate-aliases.md) — crate abbreviations; [crate-map.md](docs/reference/crate-map.md) — what each crate is for
- [documentation-layout.md](docs/reference/documentation-layout.md) — docs layout conventions
- [DOC-38](docs/specs/spec-linked-documentation.md) — SLD policy, enforced by `scripts/check_sld.sh`
- [threat-model.md](docs/reference/threat-model.md) — per-daemon bind/guard/proxy inventory ([ADR-0018](docs/adr/0018-loopback-only-doctrine.md))
- [generated-doc-regions.md](docs/reference/generated-doc-regions.md) — the `<!-- BEGIN GENERATED: … -->` contract and `UPDATE_DOCS=1 cargo test -p <crate> --test generated_docs`
- [public-manifest.tsv](docs/public-manifest.tsv) — ALLOWLIST of publishable `docs/` pages, enforced by `scripts/check_public_docs.sh`
- `scripts/check_doc_paths.sh` (#5147) — resolves backtick-quoted path citations in this file, the crate `CLAUDE.md`/`README.md` set, `docs/reference/` and `docs/architecture/`. Write a non-literal path in one of the shapes its header's EXCLUDED TOKENS list covers rather than widening the gate
