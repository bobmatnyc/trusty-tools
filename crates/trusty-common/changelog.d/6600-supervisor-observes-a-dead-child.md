Fixed

- `UdsServiceSupervisor::ensure_running` now watches the child it spawned as
  well as the socket, so a child that dies before binding is reported within one
  poll interval instead of after the whole `ServiceTimeouts::spawn_probe` window
  (#6600). The #6595 CI signature was a child that failed `FactStore::open` on a
  held redb lock in ~100 ms and surfaced 20 s later as `SpawnTimeout`, whose
  message blames the probe budget — a budget that had nothing to do with it.
- New `SupervisorError::ChildExited` carries the child's exit status and the
  last 20 lines it wrote to stderr. A supervised (non-detached) child's stderr is
  piped and copied through to this process's stderr unchanged, so the operator's
  log stream is unaffected while the tail stays quotable. A DETACHED child keeps
  `Stdio::inherit()` and reports an empty tail: it outlives the process that
  spawned it, and a pipe whose read end went with that process turns the child's
  next log write into `EPIPE`.
- `SupervisorError::SpawnTimeout` carries that same stderr tail. Its message
  guesses the cause ("its spawn_probe is too small"), and a child still loading a
  model and one spinning on a lock it will never get produce the same timeout and
  different logs.
- The stderr relay bounds each line at 8 KiB rather than only the line COUNT, so
  a child that writes a megabyte before its first newline can no longer be
  buffered and retained whole. An over-long line is passed through to stderr in
  8 KiB pieces — no bytes are lost — and the retained tail is bounded at about
  160 KiB per child. The relay also survives invalid UTF-8 rather than treating
  it as EOF, and writes each line and its terminator in one call so two children
  cannot interleave mid-line.
- A child that is alive but slow still gets the full probe window, and a
  `try_wait` that errors is treated as "still running" rather than as a death.
- One over-cap stderr line now occupies ONE slot in the 20-line ring instead of
  one slot per capped read (#6601 review). A child printing a real diagnosis and
  then a megabyte of padding left `entries=20` of padding and evicted the
  diagnosis — on the exact path this reporting exists to serve. The relay keeps
  the capped prefix with a marker naming the dropped byte count, reads on to the
  next newline without retaining it, and still relays every byte to stderr. A
  line ending exactly at the cap no longer yields an empty entry, and a codepoint
  the cap splits is dropped rather than rendered as U+FFFD.
- `ensure_running`'s documentation now states that a detached child's
  `ChildExited` and `SpawnTimeout` carry an EMPTY stderr tail: detached children
  keep `Stdio::inherit()`, so there is nothing to quote. The status and the
  one-probe-interval latency are unaffected.
