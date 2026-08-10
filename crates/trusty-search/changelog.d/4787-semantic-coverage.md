Fixed

- `GET /indexes/:id/status` gained `semantic_coverage`, carrying `vectors_present` read from the live vector store alongside the per-boot `embedded_this_boot` delta. `stages.semantic.embedded` counts only what the current boot's embed pass computed, so it reads `0` on a fully-working index whose HNSW snapshot was already current — indistinguishable from the dead-lane signature of #2178, and enough to get three healthy indexes flagged during an estate audit. `stages.semantic.embedded` keeps its name and value for wire compatibility (#4787).
