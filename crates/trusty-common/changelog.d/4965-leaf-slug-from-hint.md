Added

- `session_naming::leaf_slug_from_hint` — sanitize an operator-typed name hint to the same kebab-case leaf the daemon's `name_hint` path produces, without `leaf_slug_from_dir`'s `Path::file_name()` step (refs [#4965](https://github.com/bobmatnyc/trusty-tools/issues/4965))
  - a caller that slugifies with this before sending makes the daemon's own pass a provable no-op, so `feature/auth` and `hotfix/auth` no longer both collapse to `auth`
  - `leaf_slug_from_dir` now delegates to it, so there is one kebab-caser rather than two that can drift
  - returns an empty string when nothing alphanumeric survives, leaving the caller to choose a fallback or a refusal
