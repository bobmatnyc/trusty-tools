Added
- Every `Agent` dispatch now carries a `## Project rules (from CLAUDE.md)` block: the regions of the project CLAUDE.md marked `<!-- tm:agent-rules -->`, or the whole file when it marks none. Subagents started with `omitClaudeMd: true` keep the project build and test rules (#7684).
