Fixed
- `tm ls` auto-prune now clears an unmanaged adopted session that resolved no workspace at all. Such a record carries neither `workspace_path` nor `cwd`, so the old predicate had nothing to probe and kept it forever. A record that still names a directory is unchanged — a merely unreachable path is not an unresolvable one — and a legacy record with no adoption marker is still kept (#6118).
