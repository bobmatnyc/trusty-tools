Documentation

- `code-review-standards` (`code-critic`/`code-analyzer`) now flags a workflow
  granted `contents: write` that pushes to a protected branch with
  `GITHUB_TOKEN` — GitHub never allows the Actions identity as a restriction
  or ruleset bypass actor ([#8016](https://github.com/bobmatnyc/trusty-tools/issues/8016)).
- `code-review-standards` now flags a test helper that builds a structured
  (JSON/YAML/TOML/SQL) payload by `format!` interpolating a caller-supplied
  value into a string literal instead of the format's own builder/serializer
  ([#7624](https://github.com/bobmatnyc/trusty-tools/issues/7624)).
