---
name: architect-completion-standard
description: Architect skill. Load before clearing, closing or merging a PR or work item, before deciding whether a review finding blocks, and before setting a fix round or a delta review. Covers Done, the fix bar, the round limits and the Architect's clearance rules. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, completion, review, clearance]
effort: medium
---

# architect-completion-standard — Done and the Fix Bar

Load this before any clearance, close or fix-round decision. The bar itself
(Done, the fix bar and the round limits) is in the Architect core, which
binds you; this skill is how you apply it.

<!-- rule:r-041 -->
**Bias to completion.** Drive work to Done and ship it. A defect with little
day-to-day risk or impact never delays a merge, release or close. Send this
standard to a PM that does not follow it.

<!-- rule:r-042 -->
- **Done** means all four: the acceptance criteria are met; the required
  gates pass (tests for the change, required CI, a critic with no open
  CRITICAL or HIGH, the credential scan); a runtime change passed its rung's
  live check; and it shipped and closed with evidence. Then stop.
<!-- rule:r-043 -->
- **Good enough.** A finding blocks only if it causes or leaves unguarded:
  wrong behaviour against the acceptance criteria, a security or credential
  exposure, data loss, a crash, hang or leaked process, a resource pileup, or
  a broken gate or CI. Everything else is one comment on the PR or the
  existing issue: no new round, no new issue, no fix round.
<!-- rule:r-044 -->
- **Limits.** After the first review: at most one fix round and one delta
  review. A blocker left after that comes to you, and you decide between
  close-and-fold, a re-scope, or a question to the owner. No scope widening,
  no hunting adjacent bugs. Progress is user impact, not issue counts.
<!-- rule:r-045 -->
- **Your clearance.** Clear on Done items 1 to 3. Never make a non-blocking
  MEDIUM or LOW a clearance condition, and name over-polishing (repeated
  rounds, non-blocking fix rounds, edge-case issues) as drift in your report.
  Anything beyond this standard needs the owner's explicit request. A project
  ruling that sets a stricter bar stays in force for that project only.

<!-- rule:r-078 -->
**The fix bar decides, not the tool's severity.** The owner's fix bar
decides BLOCK, not a review tool's severity label. A finding in the fix-bar
class (a crash, hang, data loss, leaked process, overcommit or pileup, a
fail-open restart, a broken gate) blocks even when graded HIGH or MEDIUM.
Critic briefs carry the issue and the required behaviour, not the
implementer's design summary. A missing error-arm test on a new failure
branch blocks (the Fail-Open check): a fail-open arm is the hang, leak or
data-loss class the fix bar protects. Other below-BLOCK findings go in one PR
comment. A release or scope ruling names what it excludes as well as what it
includes.

## Related skills

- `architect-github`: reading a PR and its checks before a clearance.
