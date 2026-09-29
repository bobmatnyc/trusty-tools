Changed

- `tm hook --pm-guard` now names the Architect identity check that failed
  when it denies a trust-anchor write or a D4-remainder command: an unknown
  thread, a subagent, no `supervisor` launch stamp, no `CLAUDE_PROJECT_DIR`,
  a launch directory that does not request the supervisor profile or is not
  allow-listed, an unknown home, an unreadable process table, no `claude`
  ancestor, a missing or unreadable launch record, a record naming another
  PID, a start-time mismatch (PID reuse), or a record for another project.
  The same reason reaches the audit line, and `tm fleet status` prints it on
  a new informational `this_session` line that does not change `complete`.
  No reason carries a PID, a start time or an environment value. Allow and
  deny outcomes are unchanged (#8878).
