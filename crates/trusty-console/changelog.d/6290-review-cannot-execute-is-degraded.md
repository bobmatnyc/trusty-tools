Fixed

- A `trusty-review` binary that is on PATH but will not run (broken signature,
  truncated download, a hang) now reports `Degraded` with the reason as its
  hint, instead of `Available` (#6290). The non-zero exit was collapsed into the
  same `None` as "no version string", which also put the console at odds with
  `tctl`, whose presence probe calls the same host `ProbeFailed`.
