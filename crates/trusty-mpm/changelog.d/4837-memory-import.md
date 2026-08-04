Added

- `tm memory import <DIR> --palace <SLUG>` — bulk-load a directory of memory
  `.md` files into a trusty-memory palace with zero LLM inference
  (refs [#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837),
  unblocks [#4834](https://github.com/bobmatnyc/trusty-tools/issues/4834))
  - maps YAML frontmatter onto drawer fields: the `description` leads the
    stored text, and `name` + `metadata.type` + every `[[wikilink]]` target
    become tags
  - `--dry-run` reports what would be written and issues no writes;
    `--json` prints a per-file report carrying each drawer id
  - re-running is idempotent: a file is skipped when the palace already holds
    a drawer tagged with its slug whose first line is that file's headline —
    an exact tag + headline match, never `memory_recall`
    ([#4836](https://github.com/bobmatnyc/trusty-tools/issues/4836))
