Added
- A finding at a removed line with no new-side match now anchors inline with `side: LEFT` at its old-side line number; a removed line that cannot anchor for another reason reads `deleted_line` in `unanchored`, not `off_diff` (#10762).
- A finding with a file but no line, where the diff names that file, now anchors inline with `subject_type: file` and `line: 0`; a no-line finding on any other file stays `no_line` (#10762).
- The map-reduce "assessed no files" abort now partitions its withheld findings into `unanchored`, like a completed review (#10762).
