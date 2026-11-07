Fixed
- The review POST now sends each inline comment's real `side`, so a `LEFT` anchor posts on the old side; a multi-line anchor also sends `start_line` and `start_side` (#10762).
- A file-level (`subject_type: file`) or `line: 0` anchor is no longer sent in the review's `comments[]`, where GitHub rejects it and fails the whole review; its finding is listed once in the summary body instead. `inline_comments` in the `ReviewResult` JSON is unchanged (#10762).
