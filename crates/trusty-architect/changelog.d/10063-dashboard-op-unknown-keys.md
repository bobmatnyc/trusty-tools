Fixed
- `dashboard` batch ops refuse a key outside `op`, `collection`, `id`, `data`, `if_version` (exit 2), so a misspelled `if_version` fails the whole batch instead of dropping the stale-write guard ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
