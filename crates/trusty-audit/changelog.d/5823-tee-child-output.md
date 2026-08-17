Changed

- Each `tga audit` child's stdout and stderr are piped and teed rather than
  pointed straight at the log file. The per-repository log is unchanged in
  content; a repository whose child output could not be written to it is now
  recorded as a failure rather than left as a silent gap in the evidence
  ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
