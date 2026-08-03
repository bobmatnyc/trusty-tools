<!--
One PR = one primary outcome plus everything needed to ship it safely:
implementation, regression tests, necessary refactoring, docs/API updates,
changelog fragment, and in-scope review fixes. Do NOT split tests, docs,
changelog, or small review corrections into follow-up PRs. Split only when the
outcomes are independently reviewable, revertible, or releasable.

One PR may close several related issues when one coherent change satisfies them.
-->

## Outcome

Primary result and linked issue(s).

Closes #<issue-number>

## Change

Implementation summary, the important design choice, and explicit non-goals.

- **Type:** bug fix | feature | refactor | perf | docs | deps | CI
- **Crate(s) affected:** `crate-name`

## Risk / blast radius

Public contracts, persistence, security, or process-lifecycle boundaries touched.

## Test evidence

State the test **design**, not the test volume: the invariant or failure mode
being protected, and why the narrowest regression test would have failed before
this change. No new test file or fixed test count is required — choose the
contract boundary (inline unit, integration, property, or e2e).

- Regression: `<command>` — `<result>`
- Affected crate: `cargo test -p <crate>` — `<result>`
- Dependents / workspace / e2e, when the change class requires it: `<command>` — `<result>`
- Baseline failures: none | canonical issue + evidence

<!--
Scope gates to the blast radius. `cargo test --workspace` belongs at hardening
and release boundaries, not on every narrow PR. Never make a red gate green by
deleting, excluding, or `#[ignore]`-gating coverage — scope is for speed, never
for hiding a failure. Summarize passing output; attach or link raw logs for
failures, flakes, performance claims, and disputed results.
-->

## Review findings

Fixed here | retained on the parent issue/PR | promoted to a new issue (say why
it crossed the threshold).

## Docs / changelog

- [ ] Changelog fragment added at `crates/<crate>/changelog.d/<issue-or-pr>-<slug>.md`
      for **each crate whose `src/**` changed** — a source change without one fails
      `scripts/check_changelog_fragment.sh`. Docs-only, CI-only, and test-only
      PRs are exempt.
- [ ] Public API docs / reference / runbook updated, or not applicable.

## Checklist

- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy -p <crate> --all-targets -- -D warnings` clean
- [ ] `bash scripts/check_line_cap.sh` returns exit code 0
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] No breaking changes, or breaking changes are documented

---

🤖 This PR will be reviewed by `trusty-review` and merged once all checks pass.
