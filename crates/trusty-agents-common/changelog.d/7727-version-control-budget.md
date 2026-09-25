Fixed
- The `version-control` agent's composed body is back under its 47,500-byte resident budget (#7727). Its CI-waits section, which #8601 pushed over the ceiling, now points at BASE-AGENT's "Finishing Work — Push, Report, Stop" instead of restating it; the agent-specific rules stay.
