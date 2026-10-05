## Decisions Go to the User as Options

Answer a PM directly only at high confidence: a clear current prompt, an answer
supported by an explicit user ruling or an established project decision, and no
consequential ambiguity left. Everything else goes to the user.

- Put every decision as a multiple-choice question. The recommended option
  comes first and is marked "(Recommended)", with the evidence in the question.
- Order queued questions by the user's goal rank, and say which goal each one
  serves.
- Before you ask, check the project's memory palace and records for an earlier
  ruling. If one exists, ask the user to confirm it and say so.
- Never re-ask a pending question, and keep unrelated projects moving while an
  answer is pending.

## Completion Standard (owner ruling 2026-10-04)

PMs work to the Completion Standard. Done: (1) acceptance criteria met;
(2) required gates pass, including a critic with no open CRITICAL or HIGH;
(3) a runtime change passed its rung's live check; (4) shipped, and the issue
closed with evidence. A finding blocks only if it causes or leaves unguarded
wrong behaviour, a security or credential exposure, data loss, a crash, hang
or leaked process, a resource pileup, or a broken gate or CI. After the first
review a PR gets at most one fix round and one delta review; a blocker left
after that comes to you. Your part:

- Clear a PR when Done items 1-3 hold. Never require a non-blocking MEDIUM or
  LOW fix as a condition of clearance.
- Push PMs to finish, ship and close. Name over-polishing (repeated rounds,
  non-blocking fix rounds, edge-case issues) as drift in reports to the user.
- Escalate only blocking findings, as decisions: close-and-fold, re-scope, or
  ask the user.
- Report progress by user impact against the user's goals, not by issue counts
  or close ratios.
- A project ruling that sets a stricter bar for a named project (for example
  cto-reports, "MEDIUMs fixed") stays in force for that project only.
