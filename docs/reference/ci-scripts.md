# CI-only check scripts

(Plus two operator scripts no workflow runs at all — see the last section.)

Eleven scripts in `scripts/` run in a GitHub Actions workflow and nowhere else —
no pre-commit hook, no `Makefile` target, no other doc page. Each was written
for a specific failure and none of them announced itself anywhere a reader
would look, so a change to one of the workflows below could drop it with
nothing to notice.

`check_public_docs.sh` is listed as a twelfth row for the opposite reason: it ran
in a pre-commit hook ALONE until #5134, which is unenforceable (`--no-verify`, a
commit made outside the repo's hook path, a merge that runs no hook), and two
files asserted a CI job that did not exist.

Scripts that CLAUDE.md or another reference already covers are not repeated
here: `check_line_cap.sh` ([sloc-cap.md](sloc-cap.md)), `check_semver.sh`
([semver-gate.md](semver-gate.md)), `check_changelog_fragment.sh`
([changelog-fragments.md](changelog-fragments.md)), `check_sld.sh`
([DOC-38](../specs/spec-linked-documentation.md)), and
`check_generated_regions.sh` ([generated-doc-regions.md](generated-doc-regions.md)).

Every script here reads its own header first. This table says where it runs and
what it stops; the header says why it exists.

| Script | Invocation | Workflow(s) | What it gates |
|---|---|---|---|
| `check_deny_duplicates.sh` | `bash scripts/check_deny_duplicates.sh [--from-output <file>]` | `pre-publish.yml` | Counts `cargo deny check bans` duplicate-version warnings against a frozen budget. `multiple-versions = "warn"` exits 0 while reporting them, so without this the warnings are never read; the count may only ratchet down. |
| `check_test_count.sh` | `bash scripts/check_test_count.sh -- <test command...>` | `test-count.yml` | Wraps a `cargo test` invocation and refuses an aggregate of zero. A filter matching no tests exits 0 printing `0 passed; 775 filtered out` — a green run that proved nothing (#4307). |
| `check_rustdoc_links.sh` | `bash scripts/check_rustdoc_links.sh [--update-baseline] [--json <file>] [--cargo-rc <n>]` — no `--crate` flag; there is no way to scope a run to one crate | `ci.yml` (`rustdoc-links`), `pre-publish.yml` (Gate 1) | Broken intra-doc links, against a frozen baseline of pre-existing ones. docs.rs builds a release's documentation once and never rebuilds it, so a broken link is permanent for that version. Both jobs call the one script, so the flags cannot drift apart. The per-PR job was added by #5973 after eight links reached main through green PRs: the tag-triggered gate found them only at the release boundary. It is NOT a required context — promoting it wedges every open PR that predates it — so it currently reports without blocking. Since #7466 it also runs one `cargo doc` pass per feature lane declared in `scripts/rustdoc-doc-lanes.tsv`, because `cargo doc` resolves default features only and a link inside a default-off feature was invisible here until release; that file must account for every declared feature of every documented crate, so a crate that gains a feature fails the gate until the feature is placed. The per-lane examination check scores the DEFAULT lane too since #7577: that lane is run unconditionally rather than declared, so it has no members in the lane file, and the check used to skip any lane with none — a default-feature `cargo doc` that exited non-zero with no attributable diagnostic was a silent pass. #7598 applied the same scoring to the `--update-baseline` write path, which carries its own copy of that loop: the skip there let a baseline be written from a run whose default lane died unexplained, and a bad baseline outlives the run that wrote it. |
| `generate-homebrew-formula.sh` | `bash scripts/generate-homebrew-formula.sh --crate <name> --version <X.Y.Z>` | `homebrew-formula.yml`, `release.yml` | Renders `tap/Formula/<crate>.rb` for the `bobmatnyc/homebrew-trusty` tap. It is the single implementation (#5635); `release.yml` calls it instead of carrying an inline heredoc and a second copy of the crate→binary map. |
| `classify-ci-results.sh` | `CI_JOB_RESULTS=<json> bash scripts/classify-ci-results.sh` (reads `$CI_JOB_RESULTS`, falling back to its positional args) | `ci.yml`, `red-main-notify.yml` | Turns a set of job conclusions into the red-main verdict. `cancelled` is not `failure`, so the previous `contains(needs.*.result, 'failure')` test let an all-cancelled run report main verified (#4179). |
| `ci-create-local-main.sh` | `bash scripts/ci-create-local-main.sh` (no arguments) | `ci.yml`, `pre-publish.yml` | Creates the local `main` branch the `trusty-agents` git tests need, and fails the step when creation genuinely fails. The `git fetch origin main:main \|\| true` it replaced swallowed a GitHub 500 and produced an unrelated test failure eleven minutes later (#5693). |
| `detect-embedder-cuda-relevant.sh` | `git diff --name-only ... \| bash scripts/detect-embedder-cuda-relevant.sh` (reads changed paths from stdin, one per line; or set `CUDA_SCOPE_BASE=<ref>` to have it compute the diff itself) | `ci.yml` | Decides whether a change can affect the `trusty-common` `embedder-cuda` build, so the CUDA leg runs when it is relevant and is skipped when it is not. |
| `ci-crate-relevance.sh` | `bash scripts/ci-crate-relevance.sh <crate>` (changed paths supplied the same way as the script above) | `ci.yml` | Decides whether a change can affect ONE crate's build, so the four required Tauri UI clippy jobs skip their expensive steps when the diff cannot reach that crate (#7063). The closure comes from `cargo metadata --no-deps` at run time, dev- and build-dependencies included, and matching is on path segments; every error answers `true`, so a broken detector costs a full build and never a silent skip. The jobs themselves still run and report — a required context that reports nothing wedges the PR BLOCKED (#4468). |
| `check_workspace_dep_versions.sh` | `bash scripts/check_workspace_dep_versions.sh` (no arguments) | `version-parity.yml` (`pr-version-bump`) | Asserts every internal `[workspace.dependencies]` row's `version` requirement actually accepts the member crate's own version, under Cargo's caret rules. The row's `path` wins in-tree, so no cargo command in the workspace can see the drift — `trusty-console` sat at `^0.9.0` against a 0.11.0 crate and `tga` at `^6.0.1` against 7.1.0, both invisible until `cargo publish` or an external consumer resolved from the registry (#6776, same class as #4088). |
| `check_public_docs.sh` | `bash scripts/check_public_docs.sh [--stale] [--stale-terms <path>] [--manifest <path>] [--no-stale]` | `public-docs.yml`, plus the `public-docs` pre-commit hook | Validates `docs/public-manifest.tsv`, the allowlist the website publishes from: every PAGE row must resolve to an existing `.md` under `docs/`, outside the DO-NOT-PUBLISH trees, on a unique route. The STALE content pass runs in the same invocation and needs NO flag (#5134) — every published page is searched for the retired names in `docs/public-stale-terms.tsv`, whose waivers are a count ratchet in both directions. Both call sites invoke the script bare, so neither can drop the content check without deleting the gate outright. A page the gate cannot read fails as `UNREADABLE` rather than passing as clean. |
| `check_token_drift.mjs` | `node scripts/check_token_drift.mjs` (no arguments) | `token-drift.yml` | Compares each Tailwind app's hand-transcribed `--color-*` RGB triples against the canonical Foundry `tokens.css`. `ci.yml` deliberately does NOT duplicate it (`ci.yml`, `ui-checks` job): `token-drift.yml` already runs it across all seven crates directly rather than through each `package.json`. |
| `check_context_budget.sh` | `bash scripts/check_context_budget.sh [--update]` | `ci.yml` (`changes`) | Sums the bytes of `CLAUDE.md`, the framework instruction sections and the active output style against `scripts/context-budget-baseline.tsv`, and fails when the TOTAL grew more than 5% or any one file more than 10%. Every managed session pays for those three on its first assistant turn; #4513's audits measured 98k–107k tokens against a 50k target, and the creep between them was many small additions rather than one reviewable commit, so no single PR ever looked like the problem (#7424). A deliberate growth is recorded in the same PR with `--update`. It is a STEP of the existing `changes` job, not a job of its own: a new required context wedges every open PR that predates it (#5962). Reading and the trim levers: [startup-context-budget.md](startup-context-budget.md). |

## Self-tests

Ten of the twelve have a companion test that proves the gate can still fail — a
gate that cannot fail makes its own green meaningless:

| Script | Its test |
|---|---|
| `check_test_count.sh` | `scripts/check_test_count_selftest.sh` |
| `check_rustdoc_links.sh` | `scripts/check_rustdoc_links_selftest.sh` |
| `generate-homebrew-formula.sh` | `scripts/generate-homebrew-formula-selftest.sh` |
| `classify-ci-results.sh` | `scripts/check-ci-helpers-selftest.sh` |
| `detect-embedder-cuda-relevant.sh` | `scripts/check-ci-helpers-selftest.sh` |
| `ci-crate-relevance.sh` | `scripts/ci-crate-relevance-selftest.sh`, run as a step of `ci.yml`'s `changes` job before the step that consults the detector. Its closure cases run against a throwaway fixture workspace rather than this repo's graph, so an unrelated PR that adds a dependency edge does not turn them red; a short live section holds only #7063's four acceptance pairs |
| `check_token_drift.mjs` | `scripts/check_token_drift.test.mjs`, a `node:test` suite `token-drift.yml` runs before the gate |
| `check_workspace_dep_versions.sh` | `scripts/check_workspace_dep_versions_selftest.sh`, run as the step before the gate in the same job |
| `check_public_docs.sh` | `scripts/check_public_docs_selftest.sh`, run as the step before the gate in the same job. Two of its cases pass no `--stale` flag at all, so a change that made the content pass opt-in again fails there rather than going quiet |
| `check_context_budget.sh` | `scripts/check_context_budget_selftest.sh`, run as the step before the gate in the same job. Its `total_over` and `file_over` fixtures are built so that ONLY one arm can catch each — +6% on every file trips the total and not the per-file allowance, +15% on one file trips the per-file and not the total — so a change that collapsed the two arms into one fails there |

`check_deny_duplicates.sh` and `ci-create-local-main.sh` have no test of their
own. Both carry a frozen baseline or a fetch that can fail open, which is the
shape a self-test exists to pin, so both are candidates if either is edited.

**The `--gate <path>` self-test convention.** Some `scripts/*_selftest.sh`
scripts — `check_rustdoc_links_selftest.sh`,
`check_changelog_attribution_selftest.sh`,
`check_changelog_staged_selftest.sh` — accept `--gate <path>` to run their
fixture cases against an alternate copy of the gate script instead of the real
one. This is the mutation-demonstration convention: pointing `--gate` at a
deliberately broken copy proves the fixtures can still fail, the same property
"Self-tests" above states for the gate/self-test pairing itself. Not every
self-test in this file supports the flag; check the individual script's own
`--gate` handling before assuming it does.

**Reading a GitHub Actions job log from this harness.** `gh api
repos/OWNER/REPO/actions/jobs/<id>/logs` and a raw log piped through BSD `sed`
both fail here — the Bash tool refuses terminal escape sequences, and BSD
`sed`'s regex engine chokes on the raw ESC byte. Working form:

```
gh run view <run> --job <id> --log 2>&1 | LC_ALL=C tr -d '\033' | LC_ALL=C sed 's/\[[0-9;]*m//g' > <file>
```

then Read the file. `LC_ALL=C` keeps both `tr` and `sed` in byte mode so
neither trips on the ESC byte or non-UTF-8 log content.

**`gh api .../logs` needs `--allow-escape-sequences` when redirected to a
file** — without it the saved log is unreadable escape-sequence noise.

**`gh run view --log-failed` returns only `run is still in progress` while
ANY job in the run is unfinished**, even when the one job you actually want
has already completed and failed. `gh api
repos/OWNER/REPO/actions/jobs/<id>/logs` works immediately against that one
job; no need to wait for the whole run.

**Bin-target doc links need the explicit-target form.** A `` [`Name`] `` doc
link under a `src/bin/**` target that points at a library item resolves
against the bin crate's own scope, not the library's, and silently ships as
dead link text — a crate-scoped build does not catch it, only
`check_rustdoc_links.sh`'s workspace-wide run does. Spell the target
explicitly instead: `` [`Name`](trusty_mpm::path::Name) ``. Worked example:
`crates/trusty-mpm/src/bin/tm/commands/hook_payload.rs:84-85`, both forms
spelled out explicitly. (Adding a `-p <crate>` passthrough to the script so a
crate-scoped run can serve as a pre-commit gate is a separate, code-level
change, out of scope here.)

A source-scanning guard that strips comments before matching must consume
`//` and `/* */` in appearance order — stripping block comments first lets a
`src/bin/tm/**` glob inside a `//` doc line open an unterminated block
comment and silently discard the rest of the file, a false-clean ratchet.
`env_isolation_tests.rs::strip_comments` in
`crates/trusty-mpm/src/bin/tm/env_isolation_tests.rs` already does this
correctly; point any new comment-stripping guard at that precedent rather
than re-deriving the order (Refs #7568).

## Gated in a workflow, but not CI-only

`scripts/check_doc_paths.sh` (issue #5147) is deliberately NOT in the table
above: it runs in `.github/workflows/doc-paths.yml` **and** in the pre-commit
hook, so it fails the definition this page is scoped to. It is named here
because `scripts/` is where a reader looks, and the gate is new enough that
nobody has yet met it by having a commit rejected.

It resolves every backtick-quoted `crates/`, `src/`, `scripts/`, `docs/` and
`.github/` token in the live Markdown set — repo-root `CLAUDE.md` and
`README.md`, `docs/reference/`, `docs/architecture/`, and depth-1
`crates/*/CLAUDE.md` and `crates/*/README.md` — against the checkout, and fails
on any that names nothing. Its self-test is
`scripts/check_doc_paths_selftest.sh`, which the workflow runs as the step
before the gate: seven documented rules exclude placeholders, globs, elisions
and Rust module paths, and each of those is a rule that could also suppress a
real finding, so each is pinned to a fixture line under
`scripts/test-data/doc-paths/`.

Its workflow carries no `paths:` filter, which is the one thing that looks like
an oversight and is not. The citation lives in a doc and the file it names lives
in the code, so deleting or moving a source file is what breaks it — a filter
over Markdown would miss exactly that case, and filtering on the union of every
tree it can reach is every path in the repository.

## Which of these block a merge

One does: `check_workspace_dep_versions.sh` runs as a step of `version-parity.yml`'s
`pr-version-bump` job, whose context `PR version bump vs crates.io (issue #4421)`
is required, so a drifted workspace row fails a required check (#6776). The rest
did not appear in `main`'s required contexts as of 2026-09-04. Read the list live
before relying on any of them to stop a merge — a hand-copied list already cost
[#5836](https://github.com/bobmatnyc/trusty-tools/pull/5836) a merge:

```bash
gh api repos/bobmatnyc/trusty-tools/branches/main/protection \
  --jq '.required_status_checks.contexts'
```

## Operator scripts no workflow runs

Three scripts in `scripts/` exist for a person or an agent at a terminal, never
for CI. They are listed here because `scripts/` is where a reader looks, and a
script nothing invokes is a script nobody finds.

| Script | Who calls it | What it answers |
|---|---|---|
| `required-checks.sh` | `version-control`, or anyone before a merge | Prints the LIVE `required_status_checks.contexts` for a base branch, one per line. Exits 1 on an EMPTY list as well as on a `gh` failure — "nothing is required" and "the read did not work" look identical in the output, and treating either as a pass removes the last gate. `tm pr queue-check` performs the same read in-process. |
| `is-branch-caused.sh` | anyone facing a red gate | Prints `PRE-EXISTING` / `BRANCH-CAUSED` / `INCONCLUSIVE` (exit 0/1/2) for one crate. An empty `git diff --name-only origin/main...HEAD -- <crate-dir>/` settles it immediately; otherwise it re-runs `cargo test -p <crate> --no-fail-fast` in a throwaway worktree at the base ref and compares. The caller's checkout is never touched. |
| `select-test-crates.sh` | an agent or person choosing the rung-3/4 crate list for a change (#7753) | Prints, one per line, every workspace crate whose tests a given change set can affect: each crate that owns a changed file, plus the transitive reverse-dependency closure of those crates (`cargo metadata`'s full resolve graph, normal/dev/build edges all included). Default range is `origin/main...HEAD`; `--staged`, `--files <path>...` and `--range <a>..<b>` select the change set another way — a `--files` path that itself starts with `--` needs a literal `--` sentinel first. `--cargo-args` prints `-p a -p b ...` instead, with a documented override for `trusty-common`'s empty default feature set (#4901). FAIL OPEN: a `cargo metadata` failure, a missing `jq`, an empty/unresolvable range (including a bare `--range` with no value), or a bash <4 interpreter (macOS `/bin/bash`) prints every crate it can still name and exits 0 — same doctrine as `ci-crate-relevance.sh`. Exit 0 covers every well-formed and fail-open invocation; an unrecognized CLI argument still exits 2 with a usage message, a deliberate scope exclusion so a real typo stays visible (#7777 review). Requires bash 4+ for its associative arrays. Not wired into a workflow yet; see the adoption note in the issue. |

`is-branch-caused.sh` has a self-test, `scripts/is-branch-caused-selftest.sh`,
which pins the empty-diff shortcut against a synthetic repository — that path
is the one a reader acts on without re-checking, and a false `PRE-EXISTING`
would launder a real regression into someone else's problem.
`required-checks.sh` has none: it is a single `gh api` call whose only logic is
the empty-list refusal. `select-test-crates.sh` has
`scripts/select-test-crates_selftest.sh`, covering the forward-closure,
reverse-closure, non-crate-path and fail-open cases against a throwaway
fixture workspace, `--range` mode (a valid two-commit range, a missing value
bounded by `timeout`, and an unresolvable ref), a bash <4 (`/bin/bash` on
macOS) compatibility guard, and a short live check of the `trusty-common`
feature override against this repo's own graph.
