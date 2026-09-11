Added

- `host_metrics::mount_for_path` returns the usage of the mount that holds a
  given path, and `host_metrics::select_mount_for_path` exposes the
  longest-prefix selection rule behind it. A caller deciding something about one
  directory — "is the volume this worktree would land on nearly full?" — needed
  that mount rather than the cross-mount aggregate, and nothing offered the
  lookup (#7497).
- The lookup resolves the nearest EXISTING ancestor first, so it answers for a
  directory that is about to be created, and picks the mount by device id.
  The device match is what makes it correct on macOS, where `/Users/...` is
  firmlinked onto the `/System/Volumes/Data` mount and is therefore not a
  lexical child of its own mount point; prefix matching remains the fallback
  where device ids are unavailable (#7497).
- Where a device id IS readable it is authoritative, and a miss returns `None`:
  a path on a filesystem `sysinfo` does not enumerate (NFS, sshfs, some ZFS
  datasets) is reported as UNMEASURABLE rather than measured as `/`. A caller
  decides what that costs; nothing substitutes a wrong number for no number
  (#7497).
