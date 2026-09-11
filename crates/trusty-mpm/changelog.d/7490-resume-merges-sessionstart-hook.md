Fixed

- `tm session resume` and every relaunch path now merge the project-tier tm hook groups into an existing `.claude/settings.json`, so a project provisioned before an event group existed gains it instead of never firing `SessionStart` — the gap that kept the savings row and the 💸 statusline segment hidden. A new `tm doctor` check, `hooks_missing_tm_group`, reports the gap and `tm doctor --fix --yes` repairs it through the same merge (#7490).
