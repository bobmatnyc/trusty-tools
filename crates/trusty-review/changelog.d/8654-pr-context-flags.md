Added

- `trusty-review run` takes `--pr-description`, `--pr-discussion` and
  `--referenced-code` as literal text; a leading `@` is plain text. Each has a
  `-file` twin (`--pr-description-file`, `--pr-discussion-file`,
  `--referenced-code-file`) that reads a regular file of at most 256 KiB and
  conflicts with its text flag. A file that cannot be read, is not a regular
  file, or exceeds the cap fails the run before any network call.
