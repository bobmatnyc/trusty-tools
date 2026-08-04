Fixed

- Secret detector no longer flags dotted or underscored filenames that carry a capital letter (closes [#4739](https://github.com/bobmatnyc/trusty-tools/issues/4739))
  - `Agents.app.bak-20260729-000028` and shapes like it reached the mixed-case branch, which #4723's base64-branch narrowing never covered
  - `is_structural_token`'s segmented-identifier branch now splits on `.` and `_` as well as `-`, and accepts a `Capitalized` segment alongside `lowercase` and `UPPERCASE`
  - Measured over a 36-shape prose battery and a 30-shape credential battery: prose false positives 17 → 2, credential misses 0 → 0
  - CamelCase segments (`TrustyMemory.app.bak-…`) are a stated known bound, not fixed: admitting them would lose delimiter-segmented mixed-case credentials
