Changed
- The tm-managed `.gitignore` block no longer ignores all of `.claude/output-styles/`: it ignores the bundled style files and the generated `*.tm-floor.md` composites, so a project's own `<id>.md` style can be committed. An existing block is rewritten on the next launch; a hand-written line outside the block is left alone (#8533).
