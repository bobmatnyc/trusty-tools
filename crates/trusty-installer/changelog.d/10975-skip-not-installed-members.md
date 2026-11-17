Fixed
- `tctl start`, `stop` and `restart` with no member named skip a member that is not installed on the host (no binary, no plist, no loaded label) with `skipped (not installed)`, instead of failing with launchd's `Bootstrap failed: 5` or reporting a bootout of a unit that never existed ([#10975](https://github.com/bobmatnyc/trusty-tools/issues/10975))
- Naming a member that is not installed exits 2 with `<member>: not installed` and acts on no member
- A loaded launchd label whose plist is gone is refused before any bootout on `stop` and `restart`, so the running daemon is left alone; a member with no plist and no loaded label is skipped with a clear note on `stop`, `restart` and the `tctl upgrade` restart
