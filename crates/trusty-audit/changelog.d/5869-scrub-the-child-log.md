Fixed

- A credential echoed by the `tga audit` child — or by the `trusty-review` it
  spawns — no longer reaches the per-repository log file. Both piped streams are
  filtered on the way to disk through `trusty_common::credentials::scrub_secrets`,
  the workspace's one redactor, and the relay path decodes the scrubbed bytes so
  a key quoted inside a stage detail never reaches the operator's terminal either
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The needle set is every credential the process can resolve
  (`resolved_secret_values`), not only the engagement's OpenRouter key: a `gh`
  token embedded in a git remote URL is a different credential from a different
  source, and the narrow set would miss it. It removes only values this process
  already holds, so the log is lower-risk rather than proven clean
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The output pump no longer buffers a line without bound. A child printing one
  endless line with no newline is flushed in bounded pieces, holding back a tail
  as long as the longest credential so one straddling a flush is still caught
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
