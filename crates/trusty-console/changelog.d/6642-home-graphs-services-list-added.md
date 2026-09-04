Added
- Each of the four host cards (CPU, Memory, Disk, Network) draws a bar graph
  along its bottom edge, one bar per 1 s sample, newest at the right, seeded
  from the history snapshot and appended live from the SSE stream. CPU, memory
  and disk bars band at the same 80/95 (disk 85/95) thresholds the cards' own
  pressure badges use; the network graph is `rx + tx` bytes/sec scaled to the
  busiest second in the visible window
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- One `EventSource` client for the whole page, which seeds from the `history`
  snapshot, appends on `sample` and `services`, re-fetches the snapshot on a
  `lagged` event rather than appending across the gap, drops an unparseable
  frame without closing the connection, and reconnects with backoff
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
