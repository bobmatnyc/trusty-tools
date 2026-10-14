---
name: architect-completion-standard
description: Framework skill. Load before clearing, closing or merging a PR or work item, before deciding whether a review finding blocks, and before setting a fix round or a delta review. Covers Done, the fix bar, round limits and the Architect's clearance rules.
---

# Architect completion standard (framework)

Kind: framework. Index line in architect.md: load this before any clearance, close or fix-round decision.

## Completion standard
<!-- rule:r-041 -->
Bias to completion: drive work to done and ship it; a defect with little day-to-day risk or impact never delays a merge, release or close. Full text, also sent to every PM: `local/briefs/2026-10-04-completion-standard.md`.
<!-- rule:r-042 -->
- **Done** = acceptance criteria met + required gates pass (tests for the change, required CI, critic with no open CRITICAL/HIGH, credential scan) + the rung's live check for runtime changes + shipped and closed with evidence. Then stop.
<!-- rule:r-043 -->
- **Good enough (fix bar).** A finding blocks only if it causes or leaves unguarded: wrong behaviour against the acceptance criteria, security or credential exposure, data loss, a crash/hang/leaked process, a resource pileup, or a broken gate/CI. Everything else is one comment on the PR or existing issue: no new round, no new issue, no fix round.
<!-- rule:r-044 -->
- **Limits.** After the first review: at most one fix round and one delta review; a blocker left after that comes to the Architect (close-and-fold, re-scope, or ask the owner). No scope widening, no hunting adjacent bugs. Progress = user impact, not issue counts.
<!-- rule:r-045 -->
- **Architect.** Clear on Done items 1-3; never make a non-blocking MEDIUM/LOW a clearance condition; name over-polishing as drift. Anything beyond this standard needs the owner's explicit request. A stricter project ruling stays for that project only.

<!-- rule:r-078 -->
**Fix bar vs code-review-standards.** The owner's fix bar decides BLOCK, not a tool's severity label. A finding in the fix-bar class (crash, hang, data loss, leaked process, overcommit/pileup, fail-open restart, broken gate) blocks even if graded HIGH/MEDIUM. Critic briefs carry the issue and required behaviour, not the implementer's design summary. Fix bar vs Fail-Open Check: a missing error-arm test on a NEW failure branch stays blocking (the Fail-Open Check), because a fail-open arm is the hang/leak/data-loss class the fix bar protects. Other below-BLOCK findings go in one PR comment. A release or scope ruling names what it EXCLUDES as well as what it includes.
