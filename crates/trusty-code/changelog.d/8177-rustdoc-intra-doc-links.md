Documentation

- **Three broken intra-doc links resolve again.** `assets`'s module header
  gained the `crate::assets::EmbeddedAgent::Direct` reference definition its
  sibling links already had; `paths::write_dir`'s header spells
  `check_native_write_target` as `super::check_native_write_target`; and
  `provider::routing`'s `PM_MODEL_ENV_VAR` doc names `TCODE_ENGINEER_MODEL` in
  prose instead of linking a constant that does not exist. Each one failed the
  pre-publish rustdoc-links gate, which also left the `tcode` bin doc unit
  unexamined because the lib doc build aborted before reaching it.
