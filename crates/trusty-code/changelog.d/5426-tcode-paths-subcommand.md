Added

- **`tcode paths show` and `tcode paths import` (#5426).** `show` reports which
  configuration root won for each entry, the write root, and whether the private
  state directory's permissions are owner-only — `--json` for scripts. `import`
  copies `.claude/agents/**`, `.claude/skills/**`, and `.claude/settings.json`
  into `.trusty-code/`. The plan is deterministic and printed before anything is
  written (`--dry-run` stops there), and the applied report lists exactly the
  files created, so the import is reversible by deleting them. It refuses four
  classes of source rather than copying them: a target that already exists (an
  import never overwrites a user-authored file), a source that reaches through a
  symlink out of `.claude/`, a source carrying the executable bit, and a
  `settings.json` that holds a secret-bearing key or will not parse. The
  secret check splits a key into words on separators and camelCase boundaries,
  de-pluralises each word, and matches word-exactly, so `api_key`, `API-KEY`,
  `x-api-key`, `xApiKey` and `apiKeys` are all caught while `tokenizer` and
  `secretary` are not; a short whole-key exemption list keeps count-shaped
  parameters like `max_tokens` and `token_count` importable. It reads key names
  only, never values. Plugins are not copied — their
  provenance cannot be vouched for, so `.claude/plugins/` stays discoverable in
  place through the compatibility root.
