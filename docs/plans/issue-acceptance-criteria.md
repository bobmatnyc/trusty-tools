# Plan: acceptance criteria (AC) on every issue

Status: plan only, no code. 2026-11-18. Window 2026-11-18T15:45:51Z to 15:50Z. Owner instruction 2026-11-18T15:44Z.

## The AC rule (one text, reused verbatim)

> **Acceptance criteria.** Every issue states its acceptance criteria: one to four observable, testable conditions, each one a check that can fail for a named reason ("works correctly" and "tests pass" are not criteria). The criteria set the good-enough bar. When they are met and the evidence is posted, the issue closes. A stretch item beyond that bar is filed as a follow-up issue at close and linked from the closing note. It is never added to this issue's scope.

Reuse `tm-epic` "Writing acceptance criteria" (`content/skills/tm-epic.md:126-137`) by pointer: "what would pass this and still be wrong".

## Sources (under `content/`, Cargo-inert; deployed copies are not sources)
| # | Source | What changes | Rung |
|---|---|---|---|
| 1 | `content/agents/ticketing.md:79-112` ("Sparse Ticket Bodies") | Rename the "closure conditions" bullet to "Acceptance criteria"; add the rule; one line on the follow-up-at-close duty. Also the Lifecycle close step (`:387`). | 3 |
| 2 | `content/skills/tm-ticketing.md:98` + `references/ticket-content.md:3-20` | Same rule by pointer to the agent text; list "AC present" in the facts a reader must tell. `references/lifecycle.md`: close requires criteria evidence. | 3 |
| 3 | `TICKETING.md:416-430` ("Title and body"), `:198-203` (close bar) | Overrides the skill. Replace "closure conditions" with the rule; add the close interaction below. | 1 (root doc; see Q4) |
| 4 | `content/instructions/sections/core.md:31` (Completion Standard CORE, fixed section) | Done item (1) gains "stated on the issue"; one clause: stretch beyond the bar = follow-up issue. Full text `content/skills/tm-workflow/references/completion-standard.md:17-39` gets the rule. | 3 (core) |
| 5 | `.github/ISSUE_TEMPLATE/{feature_request,bug_report}.md` | Add `## Acceptance criteria`. `rust-defect.md:38` already has `## Acceptance`; align the name. Templates are human-filed paths. | 1 |
| 6 | `crates/trusty-mpm/src/core/issue_audit.rs` (REQ_* at `:282-285`, `audit_issue :313`) and `issue_audit_gh.rs:73-84` (`gh issue view --json`) | New `acceptance criteria` requirement. The audit reads no issue body today; add `body` to the fetched fields and to `IssueFacts` (`:47-53`, test `json_fields_cover_every_audited_requirement`). | 5 |
| 7 | `crates/trusty-mpm/changelog.d/<n>-ac-rule.md` | Fragment for row 6 (`src/**`). `tm issue create`/`board add` take no body check; `gh issue create` is the filing path. | n/a |

## Tests that read these texts
- `crates/trusty-mpm/src/core/pm_prompt_golden_tests.rs` with `core/testdata/pm-prompt-*.md`, `architect-prompt*.md`, `supervisor-prompt.md`: all embed `core.md`. Regenerate with `UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden` and review the diff.
- `bundled_pm_package_tests.rs`, `instruction_safety_core(_tests).rs`, `framework_content_tests.rs`, `instruction_section_sources.rs`: presence checks on the CORE sentences (spoof tests); a reworded Done clause must keep the anchor sentences.
- `bundle_fix_bar_criterion_tests.rs`: asserts the fix-bar criterion text and a `cmp` of architect skill copies (`scripts/check_architect_subproject.sh`). Do not reword the fix-bar line.
- `content/instructions/architect/core.md:92` has its own Completion Standard; it needs the same one clause, and `architect_core_tests.rs` plus `architect-prompt.md` golden follow.
- No test reads `ticketing.md`, `tm-ticketing.md` or `ticket-content.md` text (grep), so rows 1-2 add none.
- Audit (row 6): new unit cases beside `issue_audit_tests.rs`: missing AC warns; present passes; one that failed before the change.

## Conflict to resolve first
`ticketing.md:90` and `TICKETING.md:421` ban structured headings ("`##` sections = over-written") and already require "one to four observable closure conditions". So the rule is a rename plus a close duty, not a new section. Rule text above fits the sparse form: a plain "Acceptance:" list, no `##`. Templates (row 5) use `##` and conflict; see Q1.

## Close rule and TICKETING.md close stages

Close stages stay as they are (rung 1-3 close at Merged, rung 4-6 at Deployed, `TICKETING.md:198-203`). The closing note adds: each criterion with its evidence (PR sha for rung 1-3; live result for 4-6) and the follow-up issue numbers, or "none". A failed criterion blocks the close; a stretch idea does not (Completion Standard fix bar). Follow-ups are filed through the normal search-first gate, with `Refs #parent`.

## `tm issue audit` enforcement

Warn first (INFO/warn row, exit 0) for one release, then Fail (exit 1) for issues created after a cutover date, so old issues never turn red. Detection is heuristic: a line or list headed "Acceptance" or "Acceptance criteria" or "Closure". Cannot judge testability; reviewers do that. Gate on `agents.ticketing` config like the other requirements.

## Existing open issues
No backfill. An issue gains criteria when it is next touched (claimed, commented on, or transitioned). The audit's fail applies only to creation date >= cutover.

## Estimate (8 h per engineer-day; agent-hours use ~1% for mechanical, ~6% for Rust, per `trusty-browser-rust-port.md:125` actuals of ~2 h vs 31 h)

| Part | Engineer h | Agent h |
|---|---|---|
| Rule text, rows 1-5, goldens regenerated | 3 | 0.03 to 0.1 (golden/CI waits dominate) |
| Audit change, tests, fragment (row 6-7) | 6 | 0.4 |
| Review round, rung 5 critic | 2 | 0.2 |
| Total | 11 | about 0.7 |

## Open questions for the owner

1. Do the issue templates keep `##` headings for AC while ticket bodies stay sparse, or does AC become a labelled list there too?
2. Does the rule apply to all four types (bug, feature, task, epic) alike? Epics already put criteria on each phase issue.
3. Warn-to-fail cutover: one release, or a fixed date?
4. Is `TICKETING.md` (root) edited in the same PR as the skill, given it overrides it?
5. Should a follow-up filed for a stretch item be required to carry its own AC, and a "stretch" label?
