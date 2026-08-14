Fixed

- A relative `--work-dir` (or `TRUSTY_AUDIT_WORKDIR`) no longer breaks
  `trusty-audit run`. `WorkDir::resolve` anchors a relative root to the caller's
  working directory, so the pinned `tga` binary, the generated tga config, the
  output directory and the extract database are all named absolutely to the
  child process — which runs with the work-dir root as its own cwd and
  previously failed to start with `os error 2`. (#5672)
