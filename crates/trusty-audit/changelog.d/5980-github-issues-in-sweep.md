Added

- Every registered repository now automatically contributes its own GitHub
  issues to the sweep's ticketing correlation — no separate board
  registration for a repo's own tracker. The generated `tga` config for each
  repository always carries a `github:` section naming that repository, using
  the recipient's own `gh auth token` credential (never a new raw-token config
  field) when one can be read, and running unauthenticated otherwise rather
  than silently omitting the section. A repository whose issues are disabled,
  whose credential is rejected, or whose repo is invisible to the resolved
  credential (a private repo with no or an invalid token) is reported the
  same way an unreachable JIRA project already is — a named gap on the
  affected repository, not an empty-but-successful ticketing section (#5980).
  A `gh`-derived token that reaches a child never lands in the run's log or
  in the packaged deliverable unredacted, the same guarantee `boards`'
  credentials already carry.
