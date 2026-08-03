Changed

- The `require_analyze` skip message no longer tells every operator to run
  `trusty-analyze serve`. That advice was irrelevant in the default subprocess
  mode, where no analyze daemon exists at all; the message now names the real
  preconditions (a serving trusty-search and a runnable `trusty-analyze` binary
  on PATH) and scopes the daemon advice to daemon-backed deployments
  ([#4440](https://github.com/bobmatnyc/trusty-tools/issues/4440)).

---
