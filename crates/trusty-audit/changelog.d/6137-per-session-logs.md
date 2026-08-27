Fixed

- A run's child logs go into `logs/<run>/` — one directory per launch — instead
  of straight into `logs/<NN>-<repo>.log`, which `File::create` truncated. Before
  this, starting a second audit destroyed the record of the first before it had
  produced anything to replace it, so an operator comparing a failing run against
  the one that worked had nothing left to compare against; an external engagement
  reported the same loss independently. The directory is named for the local time
  and the process id and is created exclusively, so two launches in one second get
  their own; a repository carried over by the checkpoint keeps the log path from
  the run that actually audited it, and the run index links whichever that is. The
  twenty most recent runs are kept and older directories are removed as each run
  starts, so persistence does not become unbounded growth — a run that dies never
  reaches an end, so pruning at the end would not be retention (#6137)
