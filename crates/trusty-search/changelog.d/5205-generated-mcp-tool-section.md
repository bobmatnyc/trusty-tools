Changed

- The MCP tool section of `README.md` and `CLAUDE.md` is now generated from
  `tool_descriptors()` by `tests/generated_docs.rs`, from one render call that
  feeds both files, so the roster and count can no longer drift or disagree
  between them. The table gains an `Arguments` column derived from each tool's
  JSON Schema. Regenerate with
  `UPDATE_DOCS=1 cargo test -p trusty-search --test generated_docs` (#5205)
