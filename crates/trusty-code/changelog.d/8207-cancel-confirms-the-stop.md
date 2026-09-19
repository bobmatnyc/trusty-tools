Changed

- `session.cancel` now answers only once the cancelled task has actually stopped ([#8207](https://github.com/bobmatnyc/trusty-tools/issues/8207))
  - it used to return the still-`running` snapshot the instant the cooperative-cancel flag was set, so a client was told a task had stopped while it kept running and the next prompt was rejected with `-32003 invalid_argument` ("already has a task running")
  - the wait is fail-closed: a run that does not stop within 30s answers with a `-32603` error, never a cancelled snapshot
