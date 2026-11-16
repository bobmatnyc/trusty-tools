Fixed

- The research agent no longer gives up on a file the PM asks it to save. Its instructions now name the real Claude Code rule, which refuses a subagent `Write` to `REPORT*.md`, `SUMMARY*.md`, `FINDINGS*.md` or `ANALYSIS*.md`, and tell it to save such a file as `research-<name>` and report the path.
