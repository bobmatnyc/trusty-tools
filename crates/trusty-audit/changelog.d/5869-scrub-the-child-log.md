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
  endless line with no newline is flushed in bounded pieces
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The filter sees a credential in one contiguous search space, so neither of the
  two boundaries the pump used to invent can split one into unmatched halves. It
  scrubs its whole buffer before cutting a piece off to write, rather than
  scrubbing only the piece — a key lying across that cut used to reach the log in
  two verbatim halves. It also searches a mixed-encoding segment over its
  valid-UTF-8 runs concatenated, rather than one run at a time — a key is valid
  UTF-8 and so cannot contain an invalid byte, but a child can inject one into
  the middle of it, and the two flanking fragments used to reach the log in the
  clear. What the pump still holds back at a flush is only a credential that has
  not finished arriving, counted in text bytes so injected garbage rides along
  with it ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
