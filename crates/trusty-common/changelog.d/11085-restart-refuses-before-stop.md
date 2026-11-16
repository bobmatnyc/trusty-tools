Fixed
- `linux_lifecycle::restart` no longer stops a healthy daemon that its start would then refuse to replace. While the daemon serves, another process that the `/proc` scan reads as this instance now fails the restart with the new `LifecycleError::RestartBlocked` before any signal, and the running daemon stays up (#11085).
- `linux_lifecycle::restart` checks that the install binary exists before it stops anything, so a restart during an upgrade that briefly removed the binary returns `BinaryMissing` and leaves the running daemon up instead of stopping it and starting nothing (#11085).
