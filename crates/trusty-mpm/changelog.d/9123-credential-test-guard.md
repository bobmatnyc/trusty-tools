Fixed

- The bug-report token tests run `#[serial]` inside the shared credential
  sandbox, so none reads the real `~/.config/trusty-mpm/bugreport-token` and
  none prints a resolved token on failure. A new test-code scan fails CI on
  any credential test in the workspace that is not `#[serial]` and sandboxed,
  against a per-file budget that only shrinks (#9123).
