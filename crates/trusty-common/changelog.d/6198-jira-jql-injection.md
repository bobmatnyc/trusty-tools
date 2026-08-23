Fixed

- The JIRA tickets backend no longer interpolates filter values into JQL
  unescaped (#6198). `search_issues` (`query`, `assignee`, `labels`,
  `priority`) and `list_issues` (`assignee`, `labels`) built the `jql` string
  with raw `format!`, so a value containing `"` closed its quoted term and
  injected arbitrary JQL — for example an `assignee` of `foo" OR project =
  "SECRET` read issues from a project the caller could not otherwise reach.
  JQL construction now runs through pure `build_search_jql` / `build_list_jql`
  builders that escape every attacker-controlled value (backslash, double
  quote, and control characters) so a `"` can no longer break out of its term.
  `state` was never affected — it maps through a closed match to fixed literals.
