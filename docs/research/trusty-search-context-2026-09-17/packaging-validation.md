# Preservation checks

The research and experiment are preserved on `codex/search-context-experiment`; production completion remains [#8245](https://github.com/bobmatnyc/trusty-tools/issues/8245).

- Python package: 68 tests passed, including checksum verification, idempotent unpacking, changed-output preservation, and rejection of escaping paths/corrupted compressed files.
- Strict mypy: `unpack_evidence.py` passed. The original experiment previously passed its nine-file mypy gate.
- All 104 compressed evidence files unpacked with both compressed and uncompressed SHA-256 verification. Regenerating the report with `make_report.py` produced a byte-identical copy of the preserved project report.
- Gitleaks scanned the scoped publication files with archive recursion depth 3, including approximately 110 MB of expanded evidence; no leaks found.
- Largest preserved file: 915,107 bytes, below the repository 1 MB precommit limit.

Build caches, virtual environments, copied repositories, daemon state, and indexes are excluded. Historical validation counts in the original report are retained as original-run evidence; this document covers subsequent packaging checks.
