Fixed

- The GitHub tickets backend's `search_issues` no longer lets a caller-supplied
  filter inject a second search qualifier (#6216). GitHub separates issue-search
  qualifiers by whitespace, so an `assignee` or free-text `query` containing a
  space could add a live `is:`, `archived:` or `repo:` qualifier and change
  which issues came back — a `repo:` in free text widened the search past the
  configured repository. Free text is now emitted as one quoted phrase, so an
  embedded qualifier stays a search term; `assignee` is validated against the
  GitHub username grammar (plus the `*` and `@me` sentinels) because that
  qualifier takes a bare value; and a `label` is checked for the characters
  that would close its `label:"..."` wrapper. Values carrying a `"` or a
  control character are refused with a specific error rather than escaped:
  GitHub documents backslash escaping only for code search, and states that the
  syntax for issues is not the same, so there is no escape to rely on. A
  backslash is left alone, being an ordinary literal in this grammar.
