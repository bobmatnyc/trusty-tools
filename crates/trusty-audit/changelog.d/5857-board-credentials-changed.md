Changed

- The board credential reaches `tga` the way the OpenRouter key already does:
  the generated `state/tga-<stem>.yaml` carries `${TRUSTY_AUDIT_JIRA_TOKEN}` /
  `${TRUSTY_AUDIT_LINEAR_API_KEY}`, and the sweep puts the real value in the
  child's environment. No secret is written to a file. An unset variable is a
  `tga` config error naming the field, not a silent unauthenticated read.
- A board is still stated as a gap when the engagement config carries no
  credential for its provider, and when a second JIRA project is registered —
  `tga`'s `jira.project_key` holds one.
- The return package refuses a file carrying the JIRA token or the Linear API
  key, as it already did for the OpenRouter key. Those two secrets now travel to
  a `tga audit` child, and the files that child writes are the files the package
  sends off the recipient's network.
- `taudit audit` reads the target registry once. It used to read it again when
  the sweep started, hours later on a real engagement, so a board removed
  between the two reads left the report claiming coverage while nothing
  collected it — a zero exit over an engagement that skipped a registered board.
