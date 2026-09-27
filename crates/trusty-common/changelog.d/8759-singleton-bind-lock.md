Fixed

- Two daemons binding the same socket through `uds::bind_singleton_hardened` at the same time can no longer both succeed. The whole probe, takeover and bind now runs under an exclusive lock on `<socket>.lock`; a second binder that finds the lock held refuses with the new `UdsSecurityError::BindInProgress`, and one that cannot take the lock refuses with `UdsSecurityError::BindLock` instead of binding unlocked (refs [#8759](https://github.com/bobmatnyc/trusty-tools/issues/8759))
