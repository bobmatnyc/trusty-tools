Security

- State files under `.trusty-agents/` are now created owner-only (`0600`)
  instead of at the process umask, which typically left them world-readable
  (refs [#4355](https://github.com/bobmatnyc/trusty-tools/issues/4355)). These
  files hold prompts, task narratives, session records, and captured
  child-process stderr that is credential-scrubbed but not proven secret-free,
  so on a shared host they were readable by every local account.
  - The mode is set when the file is created rather than by a follow-up
    `chmod`, so there is no window in which the content exists at the wider
    mode. A temp file orphaned by an earlier crashed write is removed before
    the new one is created, so a stale `0644` temp can no longer carry its
    mode onto the published file through the rename.
  - An existing world-readable file is tightened by the next write, because
    the rename publishes the owner-only temp over it. An append-only log
    (`interactions.jsonl`, `runs.jsonl`) created before this change keeps its
    mode until it is rotated — it cannot be recreated without discarding the
    log.
  - `tasks.json` and the per-session recap files were each writing themselves
    directly and so were the two state files skipping this path entirely. Both
    now go through the shared writer, which also gives `tasks.json` the
    cross-process advisory lock the GUI, the API sidecar, and a source build
    need when they share `.trusty-agents/`. A recap summarises task
    narratives, so it was exactly as sensitive as the file it derives from.
  - No behavior change on Windows, where these paths already sit inside a
    per-user profile directory.
