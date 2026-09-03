Changed

- The hardened `git` child `local_repo` carried privately — the ambient
  `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` /
  `GIT_ALTERNATE_OBJECT_DIRECTORIES` cleared and `GIT_TERMINAL_PROMPT=0` set —
  is now `trusty_audit::git`, with a synchronous constructor beside the
  asynchronous one #6079's churn collector needs. One list of ambient variables
  rather than a copy per caller; behaviour is unchanged.
