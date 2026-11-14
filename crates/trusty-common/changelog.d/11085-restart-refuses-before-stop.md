Fixed
- `linux_lifecycle::restart` no longer stops a healthy daemon that its start would then refuse to replace. While the daemon serves, another process that the `/proc` scan reads as this instance now fails the restart with the new `LifecycleError::RestartBlocked` before any signal, and the running daemon stays up (#11085).
