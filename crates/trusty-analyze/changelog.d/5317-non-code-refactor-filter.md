Fixed

- `GET /indexes/{id}/refactor-suggestions` no longer suggests refactors for files with no mapped language. Documents, FAQs, and CI workflow YAML were scored by the keyword text heuristic, graded F, and returned as critical "extract method" suggestions (#5317).
