Fixed

- `is_ticketed` no longer counts a JIRA-shaped string found anywhere in a commit
  message. `UTF-8`, `SHA-256`, `GCC-11`, `ADR-0034` and `DOC-39` are the same
  shape as `PROJ-123`, and 237 of this repository's 2633 commits were reported
  ticketed on one of them alone. A JIRA/Linear key now counts only where the
  subject declares it, which is the rule #5199 already applied to
  `extract_ticket_id` — the two functions no longer disagree. GitHub
  action-keyword refs (`closes #N`) and Azure DevOps refs (`AB#N`) are unchanged
  and still count from anywhere in the message. Expect `commits.ticketed` to
  fall on repositories that cite documents or technical identifiers in commit
  bodies.
