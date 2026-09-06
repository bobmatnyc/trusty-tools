Fixed

- The bundled `version-control` agent told the PR-open path to write a closing keyword when the PR finishes the issue, which on a Refs-only project contradicts that project's own `CLAUDE.md`. It now writes `Refs owner/repo#N` and reaches for `Closes`/`Fixes`/`Resolves` only where a merge is allowed to auto-close, and it carries a grep it must run over the drafted body before `gh pr create` and after every body edit — a closing keyword anywhere in the body is a stop, not just one in field 1 (#6895).
