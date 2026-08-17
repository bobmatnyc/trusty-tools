Added

- `trusty-audit run` resumes an interrupted sweep instead of repeating it.
  `state/run-progress.toml` is now written after every repository rather than
  once at the end, so a crash, a timeout or a Ctrl-C costs the repositories
  still to come and none of the ones already audited
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- Each carried-over repository is named in the run's output as `resumed`, and
  the summary separates what an earlier run audited from what ran now. A
  repository being re-collected states why — its output was deleted, the
  earlier run recorded it as failed, or `--fresh` was asked for
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- `trusty-audit run --fresh` audits every selected repository again, ignoring
  the record. Reach for it when the recorded outputs are stale rather than
  missing — re-cloned repositories, or a config change that should reach work
  already done ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- A repository the record calls audited is carried over only if its output is
  still on disk and still passes the same verification that accepted it the
  first time, and only if the current selection puts it at the same output
  path. Anything else is audited again
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- `trusty-audit package` refuses a sweep that did not finish, naming the count
  recorded so far and pointing at the resume. Packaging one would have sent a
  partial engagement as a whole one
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
