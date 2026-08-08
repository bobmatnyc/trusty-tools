Fixed

- `~/.trusty-tools/trusty-mpm/config.yaml`'s `default_model` now actually affects launched sessions ([#5207](https://github.com/bobmatnyc/trusty-tools/issues/5207))
  - the trusty-console Config tab and the `config_write` MCP tool both wrote the field — under a placeholder reading "(unset — uses ~/.trusty-mpm/config.toml)" — and nothing ever read it back, so setting it did nothing. `MpmConfig::load_effective` folds it into `[models] default`, above the TOML value as that placeholder advertises and below the project file
- a misspelled key in `~/.trusty-mpm/config.toml` or `~/.trusty-tools/trusty-mpm/config.yaml` is now reported instead of silently dropped ([#5207](https://github.com/bobmatnyc/trusty-tools/issues/5207))
  - both loaders answer a parse failure by returning `Default`, so putting `deny_unknown_fields` on `MpmConfig` or `TrustyToolsConfig` would upgrade "one key is ignored" into "the whole file is ignored" — a worse failure than the one it fixes. They keep the lenient parse and gain a warning naming every dropped key path (`models.defualt`), derived from a serde round-trip so it needs no hand-maintained key list and cannot drift as sections are added
  - `Project` (`projects.json`) is deliberately left lenient too: it is machine-written, and denying unknown fields on a persisted record would make an older `tm` refuse to read a registry a newer one wrote
