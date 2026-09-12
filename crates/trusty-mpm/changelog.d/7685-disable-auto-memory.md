Added

- Disable Claude Code auto-memory for tm sessions — `trusty-memory` is the memory. A managed launch now writes `autoMemoryEnabled: false` into the project's `.claude/settings.json` and sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` on every spawned and resumed session, and a new `tm doctor` row (`auto_memory`) reports the effective setting across all four settings tiers, with `tm doctor --fix --yes` writing it. (#7685)
