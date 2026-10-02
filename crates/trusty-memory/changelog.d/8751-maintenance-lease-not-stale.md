Fixed

- `trusty-memory doctor` no longer reports a running daemon's `maintenance.lock` lease as a stale lock to remove. The `palace locks` row probes the pid the lease holder recorded: a live holder is named and never offered for removal, a dead holder is still reported as stale, and an unreadable lease or failed probe reports "cannot determine" instead of "stale" ([#8751](https://github.com/bobmatnyc/trusty-tools/issues/8751))
