Fixed

- Reindex resume-from-checkpoint (#3979, shipped in 0.42.0): four review findings on the live reindex data path.
  - The checkpoint's config fingerprint concatenated fields without unambiguous framing, so distinct walk configurations could hash identically (`exclude_globs = ["a", "b"]` collided with `["a,b"]`) and a checkpoint could be adopted for a configuration it was not built under. Fields and list elements are now length-prefixed, and paths are hashed as raw `OsStr` bytes rather than a lossy UTF-8 rendering.
  - `CHECKPOINT_SCHEMA_VERSION` bumped 1 → 2 so every checkpoint written by 0.42.0 invalidates on the version gate and falls back to a full reindex — never misread as valid under the new fingerprint.
  - The staging corpus was opened twice (once to validate the checkpoint, once to adopt it). redb locks the file exclusively, so the second open could fail and silently abandon the resume; the probe now hands its open handle to the adoption, so the file is opened exactly once.
  - "A promoted corpus never carries an in-progress checkpoint" was upheld only by the order two statements happened to appear in. Clearing the record and releasing the staging handle are now one operation that yields the token the promotion rename requires, with a fallback clear on the released file when the in-store clear cannot be confirmed.
