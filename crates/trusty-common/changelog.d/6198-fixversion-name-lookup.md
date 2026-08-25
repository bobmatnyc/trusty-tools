Fixed

- The Jira tickets backend's `get_milestone_issues` now resolves a milestone id
  to its version name before querying (#6198). JQL matches a quoted
  `fixVersion = "…"` term against the version NAME; only the unquoted form
  matches a numeric version id. The injection fix quoted every JQL term, so a
  milestone-by-id lookup silently returned no issues — or, worse, the issues of
  a version whose name happened to equal the id. The backend now reads
  `GET /project/{key}/versions`, maps the id to that version's name, and builds
  the same escaped, quoted term from the name. A caller that already passes a
  name still works, and an id matching no version is a hard error rather than an
  empty issue list, which would be indistinguishable from a real but empty
  milestone. Unquoting was never an option: it would reopen the injection.
