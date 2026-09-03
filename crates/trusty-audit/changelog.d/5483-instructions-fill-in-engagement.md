Documentation

- The handoff instructions now walk the recipient through a "Fill in the
  engagement" step, placed before install and audit: replace the placeholder
  `client`, `engagement`, and `instructions` fields in `engagement.toml`,
  register a repository with `add repo` or by pasting `[[targets]]` TOML
  directly, list and remove targets, and what `audit` does with nothing
  registered. `engagement.template.toml`'s comments mark those three fields
  "replace before running" (#5473, #5483).
