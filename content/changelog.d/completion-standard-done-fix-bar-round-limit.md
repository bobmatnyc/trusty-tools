Changed
- The PM prompt carries the owner's Completion Standard (ruling 2026-10-04): the four Done conditions, the fix bar (a finding blocks only for wrong behaviour, a security or credential exposure, data loss, a crash, hang or leaked process, a resource pileup, or a broken gate or CI), and the round limit (after the first review, at most one fix round and one delta review, then the Architect). Every other finding is one comment on the PR or issue, with no new round, issue or fix round. `tm-workflow` holds the full text.
- The round limit replaces "3+ review rounds is evidence to close and fold" in the PM prompt, `tm-workflow` and `tm-delegation-patterns`; a critic round counts against it.
- The PM prompt's Opportunistic Fixes rule now notes an easy fix in one comment instead of making it in the same work, and the QA gate and Fail-Open Check name the fix bar.
- `code-review-standards` makes `Parent` (one PR comment) the default disposition; `Fix here` is for fix-bar findings only.
- The supervisor (Architect) prompt carries the Architect's part: clear a PR when Done items 1-3 hold, never require a non-blocking MEDIUM or LOW fix, name over-polishing as drift, escalate only blocking findings.
- `code-analyzer` blocks only on a fix-bar class: Security is its own blocking priority, and Best Practices (SOLID, language idioms) is important, not blocking.
