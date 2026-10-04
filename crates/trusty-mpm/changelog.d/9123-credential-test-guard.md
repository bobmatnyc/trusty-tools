Fixed

- The bug-report token tests, the activity classifier's credential tests,
  the manager inference tests and the LLM overseer tests run `#[serial]`
  inside the shared credential sandbox, so none reads a real credential and
  none prints a resolved token on failure. A new test-code scan fails CI on
  any credential test in the workspace that is not `#[serial]` and sandboxed,
  including one that clears a credential, or loads `.env.local`, through a
  helper in the same file or in another file. Known exceptions are listed by
  test name, so the list only shrinks. The scan runs on every pull request in
  the required `Capabilities drift` job (#9123).
