Fixed

- `tm session start` from a directory outside any git repository is now refused with an actionable error instead of scattering `.trusty-mpm/`, a `CLAUDE.md` stub and a `.claude/` tier into whatever directory the shell happened to be in (refs [#4832](https://github.com/bobmatnyc/trusty-tools/issues/4832))
- session preparation no longer reads the retired `<framework_root>/instructions/INSTRUCTIONS.md`. Its content had no consumer, but any read error other than `NotFound` — a permissions fault, a directory at that path, invalid UTF-8 — raised the one fatal `PrepError` variant and killed the launch
- the `.gitignore` block `tm` writes into a target project now covers `.trusty-mpm/sessions/`, `.trusty-mpm/logs/` and `.trusty-mpm/last-instructions.md`. `framework/` and `config.toml` are deliberately still trackable — they are operator-authored config
