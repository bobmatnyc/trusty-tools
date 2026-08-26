Documentation

- `build_review_state`'s doc describes the `SubprocessAnalyzeClient` it
  actually builds — spawning the `trusty-analyze` binary per call, no daemon —
  rather than the loopback `DEFAULT_ANALYZER_URL` default that #6287 deleted.
  The broken link failed Gate 1 of the pre-publish workflow.
