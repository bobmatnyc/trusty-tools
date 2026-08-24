Fixed

- The GitHub tickets backend's `search_issues` no longer lets a caller-supplied
  filter inject a second search qualifier (#6216). GitHub separates issue-search
  qualifiers by whitespace, so an `assignee` or free-text `query` containing a
  space could add a live `is:`, `archived:` or `repo:` qualifier and change
  which issues came back. Verified against the live API: searching
  `repo:torvalds/linux bug repo:microsoft/vscode` returned 111,406 issues drawn
  from vscode, and `crash is:closed` versus `crash is:open` split 48,001 against
  3,592. Free-text terms are now quoted individually, so an embedded qualifier
  matches as literal text (both cases return 0 and 2 respectively) while
  independent-term matching is unaffected — `crash startup` returns the same
  1,663 issues it did before. `assignee` is validated against the GitHub
  username grammar (plus the `*` and `@me` sentinels) because that qualifier
  takes a bare value, and a `label` is checked for the characters that would
  close its `label:"..."` wrapper.
- A `query` or `label` carrying a `"` or a control character, and an `assignee`
  that is not username-shaped, are now rejected with a specific error rather
  than escaped: GitHub documents backslash escaping only for code search and
  states that the syntax for issues is not the same, and a trailing backslash
  before a closing quote was confirmed live to have no escaping effect. A
  backslash is left alone, being an ordinary literal in this grammar.
- `search_issues`'s MCP tool description now states these matching semantics, so
  callers can tell that `query` takes literal terms rather than search syntax.
