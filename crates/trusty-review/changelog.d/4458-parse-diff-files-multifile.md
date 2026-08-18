Fixed

- A multi-file diff no longer collapses into a single file. `parse_diff_files`
  started a file record only on a `diff --git ` line but bound the path on
  `+++ b/`, so a diff carrying `---`/`+++` pairs without those markers rebound
  the open record's path on every file and returned ONE record — named after the
  last file, holding every file's hunks. Stage A then reported `kept=1` and every
  map-reduce unit inherited the misattribution (#4458). The parser now also
  starts a record on a `---`/`+++` pair that arrives after the open record
  already has a path or hunks, and reads hunk bodies against the line budget
  their `@@` header declares so a diff-of-a-diff's body lines are not mistaken
  for file headers.
- Deleted files, binary files, mode-only changes, and pure renames now reach
  Stage A. Each of those shapes lacks a `+++ b/` line, so the old parser
  produced no record for it and the file vanished from the review with no error.
  A record's path now resolves from the first available of `+++ b/`,
  `rename to`, `--- a/`, and the `diff --git` header itself.
- Content the parser cannot attribute to any file is reported instead of
  dropped. `parse_diff_files_detailed` returns those runs as `UnparsedSection`
  entries alongside the files, and `DiffAnalyzer::analyze` logs their count at
  warn level and stamps `parsed` and `unparsed_sections` onto the Stage A line.
