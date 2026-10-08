Added
- `intent_source::extract_issue_refs` returns every issue a text links, as
  `IssueRef { number, owner_repo }`, in order of first occurrence and without
  duplicates. A ref counts only when a link keyword (`fixes`, `closes`,
  `resolves`, `refs`, `part of`, `see` and their forms) directly precedes it on
  the same line; code blocks, code spans and HTML comments are skipped, and
  `ADR-0043`, JIRA ids, `AB#N` and URLs never match (#9197).
