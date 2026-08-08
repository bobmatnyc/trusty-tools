Added

- trusty-mpm now reads a committed, project-level config file (closes [#5207](https://github.com/bobmatnyc/trusty-tools/issues/5207))
  - `<project>/.trusty-mpm.toml` is tracked in git, so a project's conventions travel with the clone, show up in PR diffs, and stop being something every operator has to re-declare on every machine. It is a ROOT-LEVEL dotfile rather than a member of `<project>/.trusty-mpm/`, because that directory holds machine-local session state and projects gitignore it wholesale — this repository's own `.trusty-mpm/*` rule already makes the #4832 `framework/manifest.toml` layer untrackable, and trusty-mpm cannot carve a re-include out of a consumer's `.gitignore`
  - `worktree` is the first setting to use it. Precedence, highest first: the project's `.trusty-mpm.toml`, then the machine-global `projects.json` registry, then the built-in `true`. The project layer wins in both directions — a repo can force isolation back ON over a local opt-out, not only off
  - the clone-based spawn branch, which asks the question before any project directory exists, is unchanged and still resolves from the registry alone
  - a key the schema does not define is REJECTED (`serde(deny_unknown_fields)`), so `worktre = false` is an error instead of silently reading as "worktrees stay on". At spawn time a rejected file contributes nothing and is logged at `error` level, and resolution falls through to the registry — a committed file is shared, so one bad push must not brick every operator's launches
  - `default_model` is the second setting: it tops the same chain `resolve_agent_model` already ends in. Explicit `--model`, per-agent overrides, and agent frontmatter are more specific than a default and still win over it

Fixed

- `~/.trusty-tools/trusty-mpm/config.yaml`'s `default_model` now actually affects launched sessions ([#5207](https://github.com/bobmatnyc/trusty-tools/issues/5207))
  - the trusty-console Config tab and the `config_write` MCP tool both wrote the field — under a placeholder reading "(unset — uses ~/.trusty-mpm/config.toml)" — and nothing ever read it back, so setting it did nothing. `MpmConfig::load_effective` folds it into `[models] default`, above the TOML value as that placeholder advertises and below the project file
- a misspelled key in `~/.trusty-mpm/config.toml` or `~/.trusty-tools/trusty-mpm/config.yaml` is now reported instead of silently dropped ([#5207](https://github.com/bobmatnyc/trusty-tools/issues/5207))
  - both loaders answer a parse failure by returning `Default`, so putting `deny_unknown_fields` on `MpmConfig` or `TrustyToolsConfig` would upgrade "one key is ignored" into "the whole file is ignored" — a worse failure than the one it fixes. They keep the lenient parse and gain a warning naming every dropped key path (`models.defualt`), derived from a serde round-trip so it needs no hand-maintained key list and cannot drift as sections are added
  - `Project` (`projects.json`) is deliberately left lenient too: it is machine-written, and denying unknown fields on a persisted record would make an older `tm` refuse to read a registry a newer one wrote
