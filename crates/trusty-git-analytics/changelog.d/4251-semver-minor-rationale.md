Breaking

- MINOR, not patch. `Config` gained a public field and its fields are all public
  with no `#[non_exhaustive]`, and the new default flips identity-resolution
  behaviour for every existing `aliases_file` deployment on upgrade.
