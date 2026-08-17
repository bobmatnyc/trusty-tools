Changed

- The board credential reaches `tga` the way the OpenRouter key already does:
  the generated `state/tga-<stem>.yaml` carries `${TRUSTY_AUDIT_JIRA_TOKEN}` /
  `${TRUSTY_AUDIT_LINEAR_API_KEY}`, and the sweep puts the real value in the
  child's environment. No secret is written to a file. An unset variable is a
  `tga` config error naming the field, not a silent unauthenticated read.
- A board is still stated as a gap when the engagement config carries no
  credential for its provider, and when a second JIRA project is registered —
  `tga`'s `jira.project_key` holds one.
