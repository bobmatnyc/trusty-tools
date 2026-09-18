# What CI Actually Gates

> Moved out of [`CLAUDE.md`](../../CLAUDE.md) by #7423, which keeps only the
> one-line headline and this pointer. Nothing here changed in the move.
>
> This file answers "which check can block my merge, and what do I do when one
> is red or absent". The per-script inventory — what each `scripts/check_*.sh`
> does and where it runs — is [ci-scripts.md](ci-scripts.md). Which gate a given
> change owes is [test-ladder-baseline.md](test-ladder-baseline.md).

## Read the required-contexts list live

🔴 **Never hand-copy it** — a stale copy cost
[#5836](https://github.com/bobmatnyc/trusty-tools/pull/5836) a merge:

```bash
gh api repos/bobmatnyc/trusty-tools/branches/main/protection \
  --jq '.required_status_checks.contexts'
```

## How the required jobs trigger

- Every required job triggers unconditionally (no `paths:` filters) and
  short-circuits on the `docs_only` boolean from the `changes` job.
- 🟡 **The four Tauri UI clippy jobs short-circuit on a second boolean (#7063).**
  The `changes` job also emits `<crate>_relevant` per UI crate, computed by
  `scripts/ci-crate-relevance.sh` from the crate's transitive workspace
  dependency closure, so a PR that cannot reach `trusty-mpm-gui` no longer pays
  for its WebKit2GTK apt chain and cargo clippy. The jobs still run and report —
  they are required contexts (#5929, #5935) — and every failure arm of the
  detector answers `true`, so a broken classifier costs a full build.
- `required_status_checks.strict` is `false`: a PR's head need not be current
  with `main` for its checks to count.
- 🟡 The required `Clippy` job runs `--workspace --all-targets` but excludes the
  Tauri UI crates (`ci.yml:34-47`); their dedicated per-crate clippy jobs must
  stay *required* to be gates
  ([#5929](https://github.com/bobmatnyc/trusty-tools/pull/5929),
  [#5935](https://github.com/bobmatnyc/trusty-tools/issues/5935)).

## A documentation change owes content gates, never a code test suite

🔴 **Two rules, owner ruling 2026-09-18.** R1: the website's code unit suite
(`Vitest (unit + smoke)`) runs for website CODE changes — anything under
`website/**` except `website/src/content/**` — and not for a changelog
fragment or a documentation change. Two Rust sources join that set because the
suite reads them and pins values out of them:
`crates/trusty-installer/src/commands/stable_set.rs` and
`crates/trusty-installer/src/download/platform.rs`. They are named exactly, not
as a `crates/*/src/**` prefix. R2: a DOCS-ONLY change never has to pass a
code test suite; it may still owe CONTENT validation (fragment format, link
and prose lints, and the changelog corpus check), which is a documentation
gate and keeps running.

**DOCS-ONLY** means every changed path matches one of `docs/**`, a repo-root
`*.md`, `crates/*/changelog.d/**`, `crates/*/README.md`, `crates/*/CHANGELOG.md`,
or `website/src/content/**`. Three path classes are **not** docs-only by
design: anything under `crates/*/src/**` **even when it ends in `.md`** —
bundled agent and skill assets are compiled into binaries with `include_str!`
and tests assert on their text — plus `.github/**` and `scripts/**`.
`scripts/detect-docs-only.sh` is the Cargo-side classifier;
`scripts/ci-website-relevance.sh` is the website-side one.

🟡 **`Refuse a test run that ran nothing` (`test-count.yml`) gates the same
way** — it wraps two live `cargo test` invocations, so a Cargo-inert diff
skips the toolchain, the cache and both cargo steps. Its shell fixtures still
run on every PR, and the job still reports. The documentation gates —
`check_sld.sh`, `check_test_pointers.sh`, the changelog-fragment check, the
path-citation lint, the public-docs allowlist — are deliberately NOT gated
this way: they are what a docs change owes.

🟡 **New check name: `Changelog corpus`** (`website-tests.yml`). The real
six-crate changelog parse moved out of the unit suite into
`website/src/lib/changelog/site.corpus.test.ts` and its own vitest project, so
a release PR's fragment no longer drags the whole website suite behind it —
PR #8272, a Rust-only fix, went red on exactly that. All three jobs in that
workflow now run unconditionally and gate only their costly steps, so the
`paths:` filters are gone; adding `Changelog corpus` to
`required_status_checks.contexts` is a branch-protection decision, made by
hand, not by this change.

## The pre-publish shards do not run on a PR

🔴 **`Rust tests (pre-publish gate)` — the eight shards — does not run on pull
requests.** Not required, skipped outright on a PR; runs on push to `main` and
`workflow_dispatch`. Do not wait for it.

🔴 **Those shards run `cargo nextest`, which gives every test its own PROCESS,
so `#[serial_test::serial]` serializes nothing there (#4162).** The `$HOME`
isolation PR #4120 established survives — a `set_var("HOME")` is process-local,
so nextest's isolation is strictly stronger than an in-process lock for env
state. What lapses is any `#[serial]` guarding a resource shared ACROSS
processes (a fixed path, a fixed port, one real file): use
`#[serial_test::file_serial]` for those, and keep redirecting `$HOME` per test.
Measurements: [test-ladder-baseline.md](test-ladder-baseline.md).

🟡 A PR proves every test target COMPILES; test EXECUTION defers to `main`, so
run the ladder rung your change earns before merging. Full suite on a branch:
Actions → CI → "Run workflow".

## A red `main` files an issue

🟡 `ci.yml`'s `notify-main-failure` opens or comments on the
`ci-red-main`-labelled tracking issue, then fails the run. A `needs:` list cannot
cross workflow files, so every OTHER push-to-main workflow is watched by
`red-main-notify.yml` over `workflow_run` instead (#5657). Adding a push-to-main
workflow means adding its `name:` to that list —
`scripts/check-red-main-coverage.sh` fails the `changes` job until you do.

## A `pull_request` run examines the merge commit, not the branch

🔴 **`pull_request`-triggered CI builds `refs/pull/N/merge`** — the PR head
merged with the CURRENT `main` tip, refreshed on every push to either side —
never the branch content alone. A local gate run against your branch proves
the branch; it does not prove what CI actually examines. Merge (or rebase
onto) `origin/main` before trusting a local green against a CI red, and expect
a failure whose cause lives in neither the branch nor its merge-base: it can
be a regression landed on `main` after your merge-base, in a crate your PR
never touched. Measured case: `./scripts/check_rustdoc_links.sh` reported 0
broken locally at `1f732b6d5`, while CI's `Rustdoc intra-doc links` job on the
same SHA reported 2 broken, both introduced on `main` by #7603 after the PR's
merge-base, in `trusty-mpm`'s `guided.rs`.

## Merge states, and what each one means

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

## Two red jobs that never block a merge

🟡 **`Public API / SemVer` and `Rustdoc intra-doc links` are not required
contexts** (verified 2026-09-07 against the protection API) — a red run there
never blocks merge. PR #6981 merged with `Public API / SemVer` and its own
break self-test failing; #6981 and #6978 both merged with `Rustdoc intra-doc
links` failing. The one actual stop for a public-API break is
`preflight-publish.sh` CHECK 5 at release, run by `local-ops`.

## Running CI's clippy locally

🔴 **A crate-scoped local `cargo clippy -p <crate>` exit 0 is not CI
evidence.** The `clippy` job in
[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) is the source of
truth for the pin, and today it installs `dtolnay/rust-toolchain@stable` — a
floating pin, not a version literal — then lints the whole workspace. A local
pass over one crate, or on a different rustc, proves neither half. PR #5488 is
the incident: local clippy green, CI clippy red.

The local `cargo` resolves to the MSRV toolchain, not CI's. On this machine
`RUSTUP_TOOLCHAIN=1.94.1` is exported into the shell, and both the rustup
proxy at `~/.cargo/bin/cargo` and the mise-shimmed `cargo` land on MSRV
regardless of `rustup default`. `rustup run <pin>` is what overrides it.

```bash
# Confirm which clippy you are about to run. 2026-09-16: clippy 0.1.98.
rustup run stable cargo clippy --version

# The CI job's own invocation, copied from ci.yml's clippy step.
rustup run stable cargo clippy --workspace --all-targets \
  --exclude trusty-mpm-gui --exclude trusty-code-gui \
  --exclude trusty-agents-ui --exclude trusty-audit-ui -- -D warnings
```

Re-read the pin from `ci.yml` each time rather than trusting this snippet:
`@stable` moves on its own, and the exclude list grows with each new Tauri UI
crate (four today). If the job is ever pinned to a literal, substitute it —
`rustup run 1.97.1 cargo clippy …`.
