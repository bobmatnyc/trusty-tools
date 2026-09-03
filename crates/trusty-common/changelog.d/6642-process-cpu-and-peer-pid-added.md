Added
- `sys_metrics::ProcessCpuSampler` measures CPU for a SET of other processes.
  `SysMetrics` only ever measured the current process, so a supervisor asking
  "how much CPU is this child using" had nothing to call. One `sysinfo::System`
  is refreshed once per tick over the tracked pids only — never the whole
  process table — and a pid whose process exits is dropped, so `cpu_pct` answers
  `None` rather than reporting a recycled pid's stranger under the old name
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- `uds::peer_pid` reads which process is on the other end of a Unix-domain
  connection (`SO_PEERCRED` on Linux, `LOCAL_PEERPID` on Darwin). A UDS daemon
  publishes no pid anywhere, so the connection a client already makes is the
  only identifier available. It returns `Option` rather than `Result` because
  every caller treats "cannot identify the peer" as a metric it does not have —
  unlike `peer_uid`, whose failure must refuse a connection
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- `host_metrics::history::MetricRing::last` borrows the newest item, so a caller
  that wants one point no longer clones the whole 600-point window through
  `snapshot()` ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
