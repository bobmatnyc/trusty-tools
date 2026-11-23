Fixed
- `tm project info` resolves a checkout registered in the persisted project registry (`projects.json` by URL or path, or `project-paths.json` by alias) after a daemon restart, instead of reporting it as not registered. An unreadable registry is now an error, not "not a registered project".
