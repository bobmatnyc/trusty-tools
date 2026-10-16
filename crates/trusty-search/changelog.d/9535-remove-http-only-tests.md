Changed
- Test-only: removed the HTTP halves of the socket stream tests, the HTTP-wire contributed-graph 503 test, and the `tests_3304` same-origin guard suite for the test-only axum router. The socket stream tests, `an_unmerged_contribution_is_refused_identically_and_stays_unqueryable`, and the shared guard tests in trusty-common and trusty-agents keep that coverage (#9535).
