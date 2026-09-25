Changed
- `trusty-memory migrate kuzu-data` is deprecated: it prints a warning and forwards to `import kuzu --from <path> --palace <name>`. `--limit` is refused, because `import kuzu` has no limit and ignoring the flag would turn a trial run into a full import; use `--dry-run` to preview.
