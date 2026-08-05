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
  - re-running never writes a file twice: the file's own drawer is found by
    its slug tag with linking drawers excluded structurally (their wikilink
    targets are re-derived), so a file whose text has drifted since it was
    imported is still recognised and skipped rather than duplicated. Never
    `memory_recall` ([#4836](https://github.com/bobmatnyc/trusty-tools/issues/4836))
  - fails closed instead of risking a duplicate or a truncated fact: an
    ambiguous candidate set, a slug tag shared by more drawers than one
    `memory_list` page returns, a frontmatter line with no `key:` separator,
    and YAML's plain multi-line scalar form are all reported as per-file
    failures rather than imported
