Fixed

- `tm status` now derives its daemon line from the same `/health` probe `tm doctor` uses, so the two can no longer disagree about reachability or pid. A daemon whose session listing is slow or erroring is reported as `sessions: listing unavailable (…) — the daemon itself answered /health` instead of `daemon: unreachable`, and the pid on the line is the one that answered rather than whatever `~/.trusty-mpm/daemon.lock` records (refs [#8025](https://github.com/bobmatnyc/trusty-tools/issues/8025))
