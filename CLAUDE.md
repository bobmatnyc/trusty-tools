# trusty-tools — Claude Code Instructions

Unified Rust workspace and single source of truth for the trusty-* AI tooling
ecosystem — shared libraries, daemon/MCP servers, orchestration harnesses,
control-plane tools, desktop clients. Derive the live package count from
`cargo metadata --no-deps --format-version 1`; do not hand-copy it here.

## Project Overview & Scope

- Resolver v2, glob members `crates/*`, MIT License. Every `crates/*`
  subdirectory with a `Cargo.toml` is an authoritative workspace member.
- Each crate owns its `version`. `[workspace.package]` shares `rust-version`,
  `edition`, `license`, `repository`, `authors` — no version field (#343).
- **MSRV `1.94`**, enforced in CI with `dtolnay/rust-toolchain@1.94`
  ([ADR-0029](docs/adr/0029-msrv-1-94-and-edition-policy.md)).
- Crate purposes: [crate-map.md](docs/reference/crate-map.md). Replaced
  repos: [former-repos.md](docs/reference/former-repos.md).

## Build and Test Commands

🔴 **Use exactly these commands, scoped to the crate you changed** — a bare
workspace run is a hardening gate, not an inner loop.

```bash
cargo build                                            # build all crates (dev)
cargo check -p <crate>                                 # fastest — no codegen
cargo test -p <crate> --no-fail-fast                   # test one crate — EVERY target
cargo clippy -p <crate> --all-targets -- -D warnings   # lint one crate
cargo fmt                                              # format (--check to verify only)
```

🔴 **`--no-fail-fast` is not optional (#5354).** Cargo stops issuing further
test targets once one target fails, so a single failing `--lib` test hides
every integration target behind it. Worked example and the incidents it
caused: [test-ladder-baseline.md](docs/reference/test-ladder-baseline.md).

- Anything else (release builds, feature-gated tests, `--include-ignored`, a
  single test by name) — `Skill(skill="cargo-commands")` rather than guessing.
- 🟡 **Crate name ≠ directory name.** `-p <crate>` takes the `name` field from
  the crate's `Cargo.toml`; exceptions are in Abbreviations & Aliases below.
- 🟡 Golden-refresh and exit-137 gotchas:
  [common-pitfalls.md](docs/reference/common-pitfalls.md).

## Rust Test Ladder

🔴 **Run the smallest deterministic gate covering blast radius; name the rung
in the PR body.** Risk maps to rung (1–2 Low, 3–4 Normal, 5–6 High).

| # | Change class | Risk | PR gate, in short |
|---|---|---|---|
| 1 | Docs, comments, changelog fragments only | Low | Doc gates only (`check_sld.sh`, `check_test_pointers.sh`, + line-cap if touched). No Cargo test and no website code suite; a fragment owes the `Website content corpus` content check. |
| 2 | Test-only stabilization — flake fix, fixture, test harness | Low | `fmt --check` + `test -p <crate> --no-fail-fast`, flake re-run ~10× |
| 3 | Localized behavior inside one crate | Normal | `fmt --check` + `check` + `clippy` + `test --no-fail-fast` `-p <crate>`, + one regression test that failed before |
| 4 | **Cross-crate change** — public API or shared library | Normal → High | Rung 3 on the library, then `SKIP_UI_BUILD=1 check --workspace` + `test -p <consumer> --no-fail-fast` for **each direct dependent** |
| 5 | Cross-crate contract, persistence, security, process lifecycle, **release tooling** | High | Rung 4 + `--include-ignored` integration coverage, failure-path tests, `code-critic` round |
| 6 | **UI / API surface** — Svelte UIs, MCP schemas, HTTP routes | High | Rung 3/4 for Rust + the UI package's test/build + one binary smoke run |

🔴 **Scope down, never scope away** — never green a red gate by deleting,
`#[ignore]`-ing, `cfg`-gating, `--exclude`-ing, or `--lib`-narrowing; a bare
`cargo test --workspace` is a publish gate, not inner-loop proof.

Per-rung commands, CI gates, baseline-red triage:
[test-ladder-baseline.md](docs/reference/test-ladder-baseline.md),
[ci-gates.md](docs/reference/ci-gates.md).

## Key Conventions

🔴 **Search before filing.** `tm-ticketing` owns the disposition on a hit
(`COMMENT` / `REOPEN` / `NEW REGRESSION` / `NO TICKET`, #5202) — search by
**test name**, **panic/error text**, **affected symbol**, and **crate**:
[issue-search-keys.md](docs/reference/issue-search-keys.md).

🟡 **Prompt-feedback rollup:
[#8021](https://github.com/bobmatnyc/trusty-tools/issues/8021).** The PM
appends the unique, actionable items from PM/agent `## Prompt feedback`
addenda there as one dated comment per session, deduplicated against earlier
comments. Never a new issue per item; never close it — strike items as they land.

🔴 **Issue lifecycle — open → in-progress → coded → merged → tested → closed.**
Four mutually exclusive labels between GitHub's native open/closed:

| Label | Meaning |
|---|---|
| `status:in-progress` | A session/agent has claimed it and is actively working it |
| `status:coded` | Implementation pushed on a branch; PR not yet merged |
| `status:merged` | PR merged to main; rung 4–6 fixes await live verification |
| `status:tested` | Verified live (installed binary / real run); eligible to close |

Claim goes on at dispatch, named session + date; reclaim only if provably
stale. Advance with `tm issue transition N status:merged`. Fix PRs use
`Refs #N`, **never** `Closes #N`. Rung 1–3 closes at merge:
`tm issue transition N closed --note "PR #M squash <sha>"`, skipping
`status:merged`/`status:tested`. Rung 4–6 (CLI/daemon/hook fixes needing
live proof) close only from `status:tested`; a merged fix failing
verification stays open, returning to `status:coded` only via a follow-up fix.
Standard of record, including agent behaviour: [TICKETING.md](TICKETING.md).

🔴 A `code-critic`/`code-analyzer`/trusty-review finding below HIGH, or a
self-improvement/post-mortem finding (the `self-improvement` label,
`tm-postmortem` output, `report_bug`/`preview_bug_report`, or an agent's
"Improvement recommendations" block), is fixed in the surfacing PR, dropped,
or logged in the rollup
([#8021](https://github.com/bobmatnyc/trusty-tools/issues/8021)) — never a
new issue. HIGH+ or independently schedulable work may still be filed
(search first).

🔴 **Why/What/Test doc pattern, proportional depth:** `/// Why: <motivation>`,
`/// What: <mechanics>`, `/// Test: <where coverage lives>`. Full pattern for
entry points and design-heavy/cross-crate code; one line for trivial items.
Defensive reasoning and ticket attribution are pointers, never narratives —
`// See <issue-or-adr>` or `// #1234: <reason>`. Enforced by
`scripts/check_test_pointers.sh`, which lints a `Test:` pointer against real
test names. `Skill(skill="documentation-style")`.

🔴 **No `unwrap()` in library code; `thiserror` for libraries, `anyhow` for
binaries**, propagated with `?`. `expect()` only for runtime-impossible
invariants.

🔴 **SLOC hard cap, mechanically enforced:** 500 non-comment/non-blank lines
for production files, 3000 for test/benchmark (basename `tests.rs`,
`_test(s).rs`, or a `/tests/`/`/benches/` segment). `scripts/check_line_cap.sh`
blocks merge on a new file over cap — split it in the same PR, never exclude
it from the count. Counting rules: [sloc-cap.md](docs/reference/sloc-cap.md).

## Per-PR Changelog Fragment (#4476)

🔴 **Every PR touching a crate's `src/**` adds a changelog fragment in the
same PR** — `crates/<crate>/changelog.d/<issue-or-pr>-<slug>.md`, never a
hand-edited `CHANGELOG.md`. A missing fragment is a review-gate failure, same
tier as a failing test (`scripts/check_changelog_fragment.sh`). Docs-only,
CI-only, test-only, and `testdata/` PRs are exempt, by file path. One
category per fragment — the first line IS the category. Validate before
committing: `bash scripts/check_changelog_fragment.sh --staged`. Format and
category list: [changelog-fragments.md](docs/reference/changelog-fragments.md).

## Git Tag / Release

🔴 **Version bumps, tags, and `cargo publish` are delegated to `local-ops`** —
the PM never edits a version file, cuts a tag, or publishes directly. Call
`Skill(skill="cargo-publish")` first: [release-workflow.md](docs/reference/release-workflow.md),
semver gate (`preflight-publish.sh` CHECK 5, always the absolute stop):
[semver-gate.md](docs/reference/semver-gate.md).

🔴 **CRITICAL macOS:** never `cp` a release binary — always `cargo install`,
or the next exec is SIGKILL'd as an invalid signature (looks like an OOM).

## Worktree Discipline

🔴 **Canonical delivery sequence:** fetch → worktree+branch from `origin/main`
→ commit (worktree only) → PR → squash-merge on green → fast-forward main
checkout → remove worktree, then delete branch. Full eight-step rule:
[worktree-discipline.md](docs/reference/worktree-discipline.md#the-delivery-sequence).
Dispatch mechanics: `Skill(skill="tm-workflow")`.

- `cargo install <crate> --version <version> --locked` — never `cp`, and
  never `--path` from a worktree, which loses provenance once that worktree
  is reclaimed ([ADR-0043](docs/adr/0043-cargo-bin-policy.md)); run from
  outside the workspace directory so no local `[patch]` resolution applies.
- **Stage by name, never `-A`:** `git add <file>` or `git add -p` — `-A`
  stages untracked build directories like `target-worktree/`.
- Docs/config stay writable in the main checkout; commits never land on
  local `main` — docs/session notes reach origin only via the fast-path PR
  ([ADR-0061](docs/adr/0061-commits-never-land-on-local-main.md)).
  Exception (owner ruling 2026-09-19): the PM may commit a NEW documentation
  file straight to `main`; the pre-push credential scan still applies.
- 🔴 **`.trusty-mpm/sessions/` is gitignored, local-only** (ruling 2026-09-13).
- 🔴 The harness (not `tm hook --pm-guard`) refuses some git/script shapes in
  a worktree — bare `git diff`, `bash scripts/…`, a heredoc. Substitute for
  every shape: [worktree-discipline.md](docs/reference/worktree-discipline.md).

## Abbreviations & Aliases

🔴 **Resolve any crate abbreviation before acting on it** — tickets, build
commands, conversation. Directory is always `crates/<crate>/`.

| Abbrev | Crate |
|---|---|
| `tga` | trusty-git-analytics (`-p tga`) |
| `tm` | trusty-memory |
| `ts` | trusty-search |
| `tc` | trusty-common |
| `ta` | trusty-analyze |
| `mpm` | trusty-mpm |
| `tagent`/`t-agents` | trusty-agents (bin `tagent`) |
| `t-agents-common` | trusty-agents-common |
| `tcode` | trusty-code |
| `tctl` | trusty-installer |
| `taudit` | trusty-audit (bins `trusty-audit`, `taudit`) |

What each crate is for: [crate-map.md](docs/reference/crate-map.md).

## Other Pointers

Dev setup (`rustup`, MSRV `1.94`+, Node/pnpm for Svelte UIs, env vars, IDE):
[environment-variables.md](docs/reference/environment-variables.md),
[ide-setup.md](docs/reference/ide-setup.md),
[running-mcp-servers.md](docs/reference/running-mcp-servers.md). Public
website — edit `website/src/content/tools/<slug>.md` for a flagship page,
never a crate README; `docs/public-manifest.tsv` is the allowlist:
[website/README.md](website/README.md). Common pitfalls (shared-capability
dedup, daemon stdout, UI build, workspace deps, global state, axum gating,
process-global env, ignored tests): [common-pitfalls.md](docs/reference/common-pitfalls.md). All
trusty-* UI builds to Foundry, not crate-local:
[docs/design/UI/](docs/design/UI/README.md), spec
[DOC-39](docs/specs/trusty-code-harness-ui.md) §8. Full reference index:
[documentation-layout.md](docs/reference/documentation-layout.md).
