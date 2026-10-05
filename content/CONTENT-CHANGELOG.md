# Content Changelog

Changes to the instructional-content bundle (`content-vX.Y.Z` releases,
ADR-0064). Crate changes are recorded in each crate's own CHANGELOG.md.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Sections are rolled up from `content/changelog.d/` by
`scripts/assemble-changelog.sh content <version>`.

---

## [2.1.1] — 2026-10-05

### Added

- The 43 agent sources (including the five compose-only `BASE-*.md`) now live
  under `content/agents/`, and the four harness-understanding docs under
  `content/instructions/harness_understanding/`, moved from
  `crates/trusty-agents-common/src/assets/` (#9011).
- Every agent declares `metadata: {version: "0.1.0"}`; `content/manifest.toml`
  lists all 43 as members under bundle version 0.2.0 (#9011).
- trusty-mpm's skills now live under `content/skills/`, its PM instruction sections, supervisor sections, instruction package and schema under `content/instructions/`, its output styles under `content/instructions/output-styles/`, its SM instructions under `content/instructions/sm_instructions/`, and the two bundled docs under `content/instructions/docs/`, moved from `crates/trusty-mpm` (#9012). The bundle paths are unchanged except the new `instructions/docs/`.
- Every skill entry point declares `metadata: {version}` (its former top-level `version:` moved under `metadata:`; `tm-doctor` starts at 0.1.0); `content/manifest.toml` lists the 57 skills as members under bundle version 2.1.0, the highest member version (#9012).

### Fixed

- `code-critic` runs a consumer suite in the foreground with an explicit timeout and never backgrounds it to wait (#8320).
- `version-control`'s no-`tm` squash fallback passes the live PR title plus ` (#N)` as the subject and the live PR body as the message; a brief never overrides them (#8420).
- `vercel-ops` warns that a bare `vercel link` can create a stray project, and that `vercel env add --force` can leave a stale duplicate (#8321).
- `local-ops` checks the operator's egress IP against the allowlist before allowlist-dependent work (#8470).
- `version-control` and `tm-workflow` describe the `tm pr merge` checks gate: a failed check, no registered check, or a running check it would not wait for refuses the merge. A branch-caused failure is never waived, and `--allow-failing <check>` waives only a non-required check the brief names (#8614).
- `version-control`, `tm-workflow` and `git-workflow` no longer let a non-required pending check through, and no longer suggest `--admin` or raw `gh pr merge` around a refusal (#8614).
- The required-checks fallback read uses the branch endpoint's `.protection` field, because `/protection` answers 404 on an unprotected branch (#8614).
- `version-control` and `git-workflow` say that a passing `tm pr queue-check` clears the queue checks only; `tm pr merge` still applies the checks gate (#8614).
- `security` states the range's `git diff --name-only --diff-filter=d | wc -l` count and the files covered in a credential-scan report. Pure-rename, mode-only and binary files count as covered and are listed separately, binaries for manual review. A mismatch makes the report INCOMPLETE, never PASS (#8504).
- `version-control` reads `gh repo view --json autoMergeAllowed` before planning a merge and reports it up front. `tm-workflow` states that auto-merge is never assumed, and that a `false` answer means a direct merge once checks settle (#8640).

### Changed

- The PM prompt carries the owner's Completion Standard (ruling 2026-10-04): the four Done conditions, the fix bar (a finding blocks only for wrong behaviour, a security or credential exposure, data loss, a crash, hang or leaked process, a resource pileup, or a broken gate or CI), and the round limit (after the first review, at most one fix round and one delta review, then the Architect). Every other finding is one comment on the PR or issue, with no new round, issue or fix round. `tm-workflow` holds the full text.
- The round limit replaces "3+ review rounds is evidence to close and fold" in the PM prompt, `tm-workflow` and `tm-delegation-patterns`; a critic round counts against it.
- The PM prompt's Opportunistic Fixes rule now notes an easy fix in one comment instead of making it in the same work, and the QA gate and Fail-Open Check name the fix bar.
- `code-review-standards` makes `Parent` (one PR comment) the default disposition; `Fix here` is for fix-bar findings only.
- The supervisor (Architect) prompt carries the Architect's part: clear a PR when Done items 1-3 hold, never require a non-blocking MEDIUM or LOW fix, name over-polishing as drift, escalate only blocking findings.
- `code-analyzer` blocks only on a fix-bar class: Security is its own blocking priority, and Best Practices (SOLID, language idioms) is important, not blocking.
- The bundle's top level is the three content classes, `agents/`, `skills/` and `instructions/` (owner ruling 2026-10-01). Output styles, the session-manager instructions and the harness-understanding docs move from top-level `output-styles/`, `sm_instructions/` and `harness_understanding/` to `instructions/output-styles/`, `instructions/sm_instructions/` and `instructions/harness_understanding/`, with the same files. `bundle-manifest.toml` lists one `[[class]]` table per class and one `[[source]]` table per packaged source directory.
- The `tm-delegation-patterns` skill says who applies a local Terraform root whose state lives in the main checkout: the operator or `local-ops` dispatched there, while a worktree agent may run `terraform plan|apply -state=<that state>` (#8660).
- The four output styles, `BASE_SM.md` and `WHAT-IS-TRUSTY-MPM.md` point an agent inside the trusty-tools repo at `content/instructions/docs/WHAT-IS-TRUSTY-MPM.md`, and `mpm-skills-manager` names `content/skills/` as the bundled skill source, where #9012 moved them; the deployed path `~/.trusty-mpm/framework/docs/` is unchanged.
- `tm-capabilities` references regenerated: the bundled docs ship with the content, and the skill catalog is read from the content's `skills/` (#9012).

