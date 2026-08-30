Fixed
- `tctl stack doctor` and `tctl stack health` no longer report a member whose
  health is `unknown` as passing. Both gained a third verdict, `undetermined`,
  with its own exit code 4 — exit 2 still means a member is genuinely broken and
  exit 0 now means every member was determined healthy. A harness gating on
  these exit codes could previously go green over a stack that was not up
  (#4847).
- A `service install` that fails because the installed binary has no `service`
  subcommand is reported as release skew instead of forwarding clap's
  `unrecognized subcommand 'service'`. `tctl start trusty-console` against the
  published 0.4.0 now names the installed version, says the member has no
  supervised unit as a result, and gives the remedy, with the raw error appended
  (#4917).
