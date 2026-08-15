Added

- Pull requests now carry their source branch (`head_ref`) and the issue their body declares (`body_ticket_id`), and both feed the same ticket extraction as commit subjects. Schema migration v24 adds the two columns; existing databases keep their rows and read the new columns as "no claim made".
- `correlate_commits` resolves a commit's ticket key through a stated precedence — the commit's own text, then the branch name of the pull request that carried it, then that pull request's body. A key the commit declares is never displaced by either new source.
- `CorrelationOutcome` reports `from_branch` and `from_pr_body`, and the summary line prints both even when they are zero, so a source that harvests nothing is distinguishable from one that was never consulted.
- `branch_ticket_key` and `pr_body_ticket_key` apply the #5199 rejection rules unchanged: a branch such as `fix/ADR-0029-followup` yields nothing, and no JIRA-shaped token is ever read from a pull-request body. Measured over this repository's 2376 merged pull requests, that keeps out 2501 false keys across 423 distinct prefixes and recovers a genuine issue reference for 51 commits whose subject declared none.
- Azure DevOps pull requests now persist the `sourceRefName` they already parsed, so the branch harvest works there too.
