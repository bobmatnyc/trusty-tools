Fixed
- `discover_claude_settings` and `discover_claude_projects` no longer enter `~/Documents`, `~/Desktop`, `~/Downloads`, `~/Pictures`, `~/Movies` or `~/Music`, and never cross onto another device; a directory whose device cannot be read is skipped. A read in an iCloud-synced folder could freeze the walk ([#10267](https://github.com/bobmatnyc/trusty-tools/issues/10267))
