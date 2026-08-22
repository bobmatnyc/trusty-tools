Added

- `env_vars::ENV_AUDIT_INVESTIGATE_MAX_FILES` and
  `ENV_AUDIT_INVESTIGATE_MAX_BYTES`. `trusty-audit` writes these variables into
  the `tga audit` child's environment and `trusty-review` reads them, so the two
  spellings have to agree and neither crate may hold a literal of its own.
