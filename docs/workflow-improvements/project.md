# Project (Rust) workflow improvements

## Recommendation

The Rust project instructions should specialize the framework policy around a large Cargo workspace. The present instructions contain strong safety conventions, but three clauses amplify ticket and test churn:

- the delivery chain is written as `spec -> issue -> worktree branch -> PR` for essentially all work;
- “each ticket, refactor, or experiment gets its own worktree” promotes experiments and refactor observations into durable workstreams;
- shared-library guidance can be read to require workspace-wide checks and all dependent suites during ordinary development, exposing unrelated PRs to a known-flaky global gate.

Keep the worktree, branch-protection, changelog-fragment, and no-coverage-deletion rules. Change the unit of work from **ticket** to **PR outcome**, and define a Rust-specific test ladder.

## Project-specific findings

Window and source are the same as the framework report: 2026-07-02 through 2026-08-02 inclusive, with a partial August 2.

- 1,655 issues were created versus 1,044 merged PRs: **1.59 new issues per merged PR**.
- The weekly ratio rose from 1.33 to 1.95, then 4.16 in the four-day final interval.
- 123 issue titles contained `test`, 33 contained `flaky`, and 67 PR titles contained `test`.
- Path-level inspection of 1,029 non-empty landed commits found 439 touching both `src/` and recognizable test paths, 376 touching `src/` without a separately recognizable test path, three test-path-only, and 211 neither. Rust inline tests can live in ordinary source files, so this is **not** a coverage verdict; it shows why file-presence heuristics must not be the project test policy.
- Of 1,044 squash commits, 36 contained at least three GitHub references. Examples include one PR fixing five non-hermetic fixtures and several PRs closing two or three related issues. Related tickets are already being delivered as one Rust change; the instructions should explicitly permit that shape.

The test problem is also real. [#3569](https://github.com/bobmatnyc/trusty-tools/issues/3569) reported about a 10.5% Test-job failure rate in a sample of roughly 500 recent runs, with 43% of failures tied to known isolation defects. The correct response is to fix and centrally track flaky infrastructure, while avoiding repeated issue creation every time an unrelated PR encounters the same baseline failure.

## Changes to `.trusty-mpm/INSTRUCTIONS.md`

### 1. Replace ticket-per-workstream language

Replace:

> Each ticket, refactor, or experiment gets its own worktree.

With:

> Each independently reviewable PR outcome gets one branch and worktree. Multiple related tickets may share that worktree when one coherent change satisfies them. Refactor steps, tests, documentation, and review fixes required by that outcome stay in the same worktree and PR. Experiments remain session-local unless their result is accepted for implementation; only then promote the accepted outcome to an issue/PR workstream.

Also change the delivery chain from an unconditional `spec -> issue -> ...` to:

```text
accepted outcome -> optional issue -> worktree branch -> one cohesive PR
                 -> applicable Rust gates -> review -> squash-merge -> cleanup
```

An issue is optional for docs/CI/chore work and for a small fix explicitly requested and completed in one PR. It remains required for features, reproduced defects, security work, cross-release dependencies, or work that must survive the current session.

### 2. Add a project ticket-boundary rule

Insert under Key Conventions:

> **Rust issue boundary:** file one issue per independently prioritizable behavior or invariant, not per failing test, module, crate touched, reviewer observation, or implementation step. Group failures that share a root cause and acceptance test. Before filing, search open and recently closed issues by test name, panic text, affected symbol, and crate. Add a new occurrence to the canonical issue when one exists.

Concrete examples to include:

- **Group:** repeated failures of `test_connect_retry_recovers_on_second_attempt`; [#2238](https://github.com/bobmatnyc/trusty-tools/issues/2238), [#2331](https://github.com/bobmatnyc/trusty-tools/issues/2331), and [#2634](https://github.com/bobmatnyc/trusty-tools/issues/2634) should have been one canonical defect with an occurrence log.
- **Keep separate:** [#1913](https://github.com/bobmatnyc/trusty-tools/issues/1913), [#1916](https://github.com/bobmatnyc/trusty-tools/issues/1916), and [#1918](https://github.com/bobmatnyc/trusty-tools/issues/1918) have different harmful outcomes and acceptance tests.
- **Do not promote yet:** a code-only risk explicitly “not confirmed as an active bug,” such as [#1914](https://github.com/bobmatnyc/trusty-tools/issues/1914), stays on the parent issue/PR until reproduced or explicitly prioritized.

### 3. Add a Rust test ladder

Replace blanket language with the smallest deterministic gate that covers the blast radius. Required tests stay in the implementation PR.

| Change class | Development proof | PR gate | Hardening/release gate |
|---|---|---|---|
| Docs/comments/changelog only | Render/link or script check if applicable | No Cargo test by default | Required CI only |
| Test-only stabilization | Repeated targeted test demonstrating fail-before/pass-after or stability | Affected crate suite; repeat/concurrency run appropriate to the flake | Workspace gate if shared test infrastructure changed |
| Localized crate behavior | Targeted regression test | `cargo fmt --check`; `cargo test -p <crate>`; crate-scoped clippy/check as applicable | Workspace gate only when release policy requires it |
| Public API or shared library | Targeted regression plus `cargo test -p <lib>` | `cargo check --workspace`; tests for directly affected consumers; applicable clippy | Full workspace tests during HARDEN/release |
| Cross-crate contract, persistence, security, process lifecycle, release tooling | Targeted and failure-path tests | Affected and dependent suites; adversarial review; relevant integration/e2e proof | Full applicable workspace, audit, and release gates |
| UI/API behavior | Rust crate tests plus direct UI/API evidence | Relevant frontend/API suite and smoke test | Full product/e2e gate when hardening |

Do not use `cargo test --workspace` as the default inner-loop proof for a localized change. It is valuable at hardening boundaries, but making every narrow PR depend on the whole workspace turns unrelated flakes into issue factories.

### 4. Define baseline-failure handling

Add this protocol:

1. A failed required gate is never ignored or converted to green by deleting, excluding, or `#[ignore]`-gating coverage.
2. Determine whether the failure is caused by the branch using a base-branch run, the failing test's history, or a focused reproduction.
3. If branch-caused, fix it in the PR.
4. If pre-existing and already tracked, append the run, SHA, command, and failure signature to the canonical issue. Do not open another issue.
5. If pre-existing and untracked, create one canonical issue only after reproduction or sufficient CI evidence. A single unrelated red run is an observation, not automatically a new ticket.
6. Report the PR gate as “change-specific gates pass; workspace gate blocked by canonical issue #N,” never simply “all tests pass.” Merge disposition follows branch protection and the risk tier.

This preserves the existing hard line against coverage deletion while preventing duplicate flake tickets and ritual reruns.

### 5. Make test design explicit, not test volume

For every behavior-changing Rust PR, require the PR body to state:

- the invariant or failure mode being protected;
- the narrowest regression test and why it would fail before the change;
- affected-crate suite result;
- dependent/workspace result when required by the ladder;
- ignored tests intentionally run, if the affected code is behind that boundary;
- untested behavior and reason.

Do not require a new test file, a fixed test count, or full raw output. Inline unit tests, integration tests, property tests, and e2e tests are chosen by contract boundary.

### 6. Narrow the inline ticket-comment rule

The current project convention asks for an inline `// #1234` pointer whenever code is modified because of a ticket. That creates unnecessary coupling between ordinary implementation and issue granularity.

Change it to:

> Add an inline issue/ADR pointer only when future maintainers need external context to preserve a non-obvious invariant, compatibility constraint, security decision, or workaround. Do not add a ticket pointer to every edited function or obvious fix; git history and the PR carry normal attribution.

This keeps useful one-hop context without making ticket creation feel prerequisite to every code edit.

## What one Rust PR should include

A normal PR should contain:

1. One primary behavior/outcome, with all related issue links.
2. Implementation and necessary local refactoring.
3. Regression tests at the correct contract boundary.
4. Error-path and concurrency tests when those are part of the failure mode.
5. Public API docs and relevant reference/runbook updates.
6. One changelog fragment per changed publishable package when user-visible source changed; retain the current docs/CI/test-only exemptions.
7. Review fixes that are necessary for correctness or acceptance.
8. Concise evidence from the Rust test ladder.

Do **not** split tests, docs, changelog, or small review corrections into follow-up PRs. Split when there are independently reversible outcomes, different release order, materially different risk/owners, or a stack makes review safer.

## Proposed Rust issue template

```markdown
## Outcome / impact
What user or system behavior is wrong or desired?

## Confidence
Observed | Reproduced | Inferred | Speculative

## Evidence / reproduction
Minimal command, inputs, failure signature, and affected SHA/environment.

## Root-cause relationship
Canonical issue searched? Same root cause as another symptom? Parent/epic?

## Acceptance
Externally observable behavior and regression test required for closure.

## Test level
Targeted | crate | dependents | workspace | integration/e2e
```

Do not require exhaustive code-location inventories or pre-solve the implementation. Add exact locations when they materially reduce ambiguity.

## Proposed Rust PR template

```markdown
## Outcome
Primary result and linked issue(s).

## Change
Implementation summary, important design choice, and explicit non-goals.

## Risk / blast radius
Crates, public contracts, persistence/security/process boundaries.

## Test evidence
- Regression: `<command>` — `<result>`
- Affected crate: `<command>` — `<result>`
- Dependents/workspace/e2e when required: `<command>` — `<result>`
- Baseline failures: none | canonical issue and evidence

## Review findings
Fixed here | retained on parent | promoted to issue with threshold reason

## Docs / changelog
Updated, exempt, or not applicable with reason.
```

Passing-output evidence should be summarized. Attach or link raw logs for failures, flakes, performance claims, and disputed results.

## Keep these existing rules

- main checkout remains inspection-only;
- feature branch/worktree and squash-merge discipline;
- never make red green by deleting or excluding coverage;
- 500/3000 SLOC caps and ratchet;
- `thiserror` for library boundaries and `anyhow` for binaries;
- per-PR changelog fragments rather than shared `CHANGELOG.md` edits;
- stronger dependent and workspace gates for shared/high-risk changes;
- explicit security review for secrets and trust-boundary changes.

The objective is not less testing. It is fewer redundant artifacts, deterministic test scope, and one reviewable delivery unit per outcome.
