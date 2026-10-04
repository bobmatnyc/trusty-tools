Fixed

- The bug-report token tests, the activity classifier's credential tests and
  the manager inference tests run `#[serial]` inside the shared credential
  sandbox, so none reads a real credential and none prints a resolved token
  on failure. A new test-code scan fails CI on any credential test in the
  workspace that is not `#[serial]` and sandboxed, including one that clears
  a credential through a same-file helper. Known exceptions are listed by
  test name, so the list only shrinks. The scan runs on every pull request in
  the required `Capabilities drift` job (#9123).
