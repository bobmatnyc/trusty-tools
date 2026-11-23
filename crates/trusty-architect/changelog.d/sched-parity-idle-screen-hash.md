Fixed
- `trusty-architect-sched parity` pairs `idle` alerts and wakes on session, kind and screen hash, when their `since-<epoch>` parts differ by at most 300 s, one to one; the report prints how many idle pairs matched this way.
- `trusty-architect-sched parity` leaves `ctxwatch` alerts and wakes out of the match and counts them on their own line, since both pollers share one debounce state file.
