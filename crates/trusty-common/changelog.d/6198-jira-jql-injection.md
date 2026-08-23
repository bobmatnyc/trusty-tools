Fixed

- The JIRA tickets backend no longer interpolates filter values into JQL
  unescaped (#6198). Four methods built the `jql` string with raw `format!`:
  `search_issues` (`query`, `assignee`, `labels`, `priority`) and `list_issues`
  (`assignee`, `labels`) quoted their terms but did not escape, so a value with
  a `"` closed the term and injected clauses; `get_milestone_issues`
  (`fixVersion = {id}`) and `get_epic_issues` (`parent = {epic_id}`) were worse
  — unquoted, so an `id`/`epic_id` of `1 OR project = SECRET` injected a live
  clause with no quote-breakout needed, reading issues from a project the caller
  could not otherwise reach. All four now build via pure `build_*_jql` helpers
  that route every attacker-controlled value through a new `escape_jql_string`
  (escapes backslash, double quote, and control characters) and quote every
  term. `list_epics`' config-derived project key is escaped too for uniformity.
  `state` was never affected — it maps through a closed match to fixed literals.
