Added

- An interrupted reindex now resumes from where it stopped instead of redoing
  the entire walk/parse/embed. Every non-`force` reindex stamps a versioned
  checkpoint record into the staging corpus it is building; after a crash the
  next reindex validates that record and adopts the staged corpus, so the
  batches already committed — and the hours of embedding they represent on a
  large index — survive
  ([#3979](https://github.com/bobmatnyc/trusty-tools/issues/3979)).
  - The resume unit is the file, and the skip stays content-verified: an adopted
    file is still re-read and re-hashed against its current bytes before it can
    be skipped, so a file edited, deleted, or created during the interruption is
    handled exactly as on a clean run.
  - Every ambiguous case falls back to a full reindex, never a partial one — an
    unopenable staging corpus, a missing or corrupt record, a schema-version
    bump, a different index id, a moved walk root, a daemon upgrade, a changed
    walk config, or a record past its adoption window. The live corpus is never
    read, written, or deleted by the resume path.
  - New optional knobs: `TRUSTY_REINDEX_RESUME` (set to `0` to restore the
    previous always-rebuild behaviour) and
    `TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS` (default `86400`; `0` disables the
    age gate).
  - `--force` reindexes deliberately neither write nor consume a checkpoint, and
    a resumed run always runs the vector catch-up pass so semantic search is not
    short the chunks it inherited.
