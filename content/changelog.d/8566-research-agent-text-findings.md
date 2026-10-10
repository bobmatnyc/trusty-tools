Fixed
- The `research` agent returns its findings as text in its final response instead of saving them under `docs/research/`, which the harness refuses for subagents; it writes a file only when the PM brief names the path (#8566).
