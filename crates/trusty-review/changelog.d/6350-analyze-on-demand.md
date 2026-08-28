Changed
- `report --analyze` starts trusty-analyze itself rather than requiring a
  resident daemon, and restarts it if it idles out mid-report: a request that
  finds the socket gone respawns the server and retries exactly once. A timeout
  does not retry — the server answered and is working. A start that fails is
  reported through the "falling back to scan" line on stderr plus an
  unassessed-dimension entry under Gaps & Caveats, never silently degraded to a
  clean-looking report (#6350).
