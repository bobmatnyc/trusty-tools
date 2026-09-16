Added
- `.trusty-mpm.toml` gains `documents_only`. A repository that declares it admits source-classified writes and commits in its own main checkout, so a prose repo whose CLAUDE.md forbids worktrees can land a `.py` rename it previously could land by neither route (#7905). Absent, `false`, unreadable or malformed leaves the ADR-0044 boundary exactly as it was.
