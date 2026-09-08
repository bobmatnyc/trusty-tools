Fixed

- The operator's home-directory path no longer reaches a client-facing member
  of an engagement package. `package.toml`'s per-repository `gaps`, the same
  gaps in `reports/index.md`, and each packaged `manifest.toml`'s
  `[[repositories]] path` now render `~` where the absolute
  `/Users/<operator>/...` prefix used to appear, so the recipient no longer
  learns the auditing operator's local account name once per repository. The
  substitution lives in one new module, `redact`, whose `home_paths`,
  `home_paths_under`, `home_paths_in` and `packaged_manifest` are the crate's
  only home-path redaction; nothing else about a gap's text changed
  (issue #7137).
