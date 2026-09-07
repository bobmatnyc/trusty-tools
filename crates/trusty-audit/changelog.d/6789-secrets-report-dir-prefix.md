Fixed

- The secrets collector now names each raw-report directory under a prefix
  scoped to the scanning process AND thread — `trusty-audit-secrets-<pid>-<thread>-`
  rather than one stem shared by every scanner. The private-directory test proves
  nothing survives a scan by differencing the shared `std::env::temp_dir()`
  listing around its own, and with one prefix for everybody that difference
  picked up directories written by sibling tests whose sweeps scan for real, so
  it failed intermittently under the parallel test harness. The `#[file_serial]`
  lock added earlier could not close it: it excludes only a second copy of that
  same test, and the collisions observed were sibling threads inside one test
  binary, which also share a pid. Filtering the listing on the scanner's own
  prefix makes the difference exact against any other scanner, in this process or
  another (issue #6789).
