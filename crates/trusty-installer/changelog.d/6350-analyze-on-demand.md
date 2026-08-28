Changed
- `tctl` no longer expects trusty-analyze to be a resident daemon.
  `member_has_service_install` excludes it (its `service install` subcommand is
  gone), and `tctl up`'s analyze stage now starts it on demand and pings
  `analyze.health` once instead of gating on a live dial — so a correct
  installation with nothing listening reports Ok rather than Degraded (#6350).
- The health probe follows. A retired member is `ManageStrategy::None`, which
  would have made analyze `Unprobeable`; it keeps its transport probe through
  the same named-predicate arm #6290 added for a presence-only member, because
  starting it and asking `analyze.health` is the only check that separates
  "working" from "broken" for a service whose healthy resting state is not
  listening (#6350).
