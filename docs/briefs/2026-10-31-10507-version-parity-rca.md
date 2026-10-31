# #10507 version-parity drift: RCA and fix plan (read-only, 2026-10-31)

Read at origin/main 828560d574. Issue #10507 is still open; 35 comments, the last at 06:28Z.

## 1. The gate

| Item | Value |
|---|---|
| Workflow | `.github/workflows/version-parity.yml`, job `version-parity` (push to main, plus cron every 3h) |
| Script | `scripts/check-version-parity.sh` -> `publish-guard` (`crates/trusty-publish-guard`) |
| Check | For each publishable crate, compare local `src/` with the crates.io tarball of the crate's own Cargo.toml version |
| Version not on crates.io | PASS (pre-release state) |
| Source differs | FAIL: unpublished drift under a published version |
| Registry unreachable | FAIL (fails closed) |
| Current state | GREEN. Every run on main since 10:48Z passed; latest push run 41247149783 at 828560d574, 22:12Z |
| Last red run | 41188381940 (schedule, 09:21Z, 5faa55f828) |

The red run reported exactly one drifted crate: `trusty-mpm 1.11.1` (about 20 modified files under `src/bin/tm`, `src/core`, `src/tui`, and 4 added test files). It had 0 unverifiable crates out of 22 checked.

Fix: commit 68cf62e403 (05:41 -04:00, #10613) changed trusty-mpm `src/` and bumped it to 1.12.0 in the same commit. Version 1.12.0 is not on crates.io, so the gate passes. The red lasted from the earlier mpm src merges until that bump. The cron runs at 03:23Z, 06:28Z and 09:21Z were red for the same cause.

## 2. Per-crate state (main vs crates.io vs tag)

Only crates where main differs from crates.io or the tag are listed. All others match.

| Crate | main | crates.io | latest tag | Cause | Gate effect |
|---|---|---|---|---|---|
| trusty-common | 0.67.5 | 0.67.4 | 0.67.4 | Bump merged, not published | PASS (0.67.5 not live) |
| trusty-mpm | 1.12.0 | 1.11.1 | 1.11.1 | Bump merged, not published. The drift was src merged under 1.11.1 before 68cf62e403 | PASS since 68cf62e403 |
| trusty-search | 0.62.2 | 0.62.1 | 0.62.1 | Bump merged, not published | PASS |
| trusty-analyze | 0.13.0 | 0.12.7 | 0.12.7 | Bump merged, not published | PASS |
| trusty-console | 0.13.0 | 0.12.6 | 0.12.6 | Bump merged, not published | PASS |
| trusty-review | 0.39.0 | 0.38.5 | 0.38.5 | Bump merged, not published | PASS |
| trusty-code | 0.7.1 | 0.7.0 | 0.7.0 | Bump merged, not published | PASS |
| trusty-installer | 0.17.1 | 0.17.0 | 0.17.0 | Bump merged, not published | PASS |
| trusty-embedderd | 0.4.2 | 0.4.1 | 0.4.1 | Bump merged, not published | PASS |
| trusty-embedderd-py | 0.2.2 | 0.2.0 | 0.2.0 | Bump merged, not published | PASS |
| trusty-agents / trusty-events / trusty-sld-lint | 0.39.4 / 0.1.0 / 0.1.0 | not on crates.io | none | Never published (not checked further whether intentional) | n/a |

trusty-secrets (0.4.0) and trusty-memory (0.36.4) are at parity on all three. No crate has a publish without a tag, and none shows a hand edit. The "secrets+1" and "trusty-common 0.67.5 vs 0.67.4" items from earlier context are the normal "bumped, awaiting publish" state, not drift.

Root cause: a single event. trusty-mpm `src/` merged to main under the already-published 1.11.1 without a bump. This slipped past the PR-time guard (`pr-version-bump`). Cause of the slip is not confirmed; the PR guard's own run for the offending PR was not inspected. The 10 other crates ahead of crates.io are the release-train backlog, not drift.

## 3. Gate and process defects

| # | Defect | Location |
|---|---|---|
| a | The tracking issue never closes. `notify-main-drift` has a single step that runs only when `needs.version-parity.result != 'success'`; nothing closes the issue on green. #10507 has stayed open for hours while green | `.github/workflows/version-parity.yml:330-340` (no `state: 'closed'` anywhere in the file) |
| b | Each failed run appends a full comment (35 on this issue), with no de-duplication of identical text | same job |

No script bug found: `check-version-parity.sh` behaved as designed.

## 4. Fix plan

1. Close #10507 now as resolved by 68cf62e403 (parity green on 8+ consecutive runs). No code change is needed for parity.
2. Optional, in a rung-3 PR: add a "close tracking issue when result == success" step in `version-parity.yml` near line 340 (defect a). Changelog fragment not needed for a workflow-only change.
3. Release train, local-ops only, owner approval required for publishing (per CLAUDE.md, tags and `cargo publish` go through local-ops). Order:
   1. trusty-common 0.67.5 first (root of the dependency graph; dependents resolve it from crates.io when published standalone).
   2. Then other libraries that depend on it, in dependency order: trusty-embedderd, trusty-embedderd-py, trusty-search, trusty-analyze, trusty-review, trusty-code, trusty-installer, trusty-console.
   3. trusty-mpm 1.12.0 last (consumes most of the others).
   4. Tag each `<crate>-v<version>` after its publish; confirm with `scripts/check-tag-publish-parity.sh`.
4. Re-run `version-parity` (workflow_dispatch or next cron) after the publishes and confirm it stays green; publishing moves the crates.io side, so a crate whose src changed after its bump would turn red (issue #4688 mechanism).

Order of dependencies in step 3.2 is inferred from names, not verified; local-ops should confirm with `cargo metadata` / `preflight-publish.sh`.

## Prompt feedback
- The brief said parity is red "now"; it has been green since 10:48Z. Stating the check time would have avoided that premise.
