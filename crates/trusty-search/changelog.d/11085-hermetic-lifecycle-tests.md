Changed
- The Linux lifecycle AC6 test scans an injected process table of its own processes plus a foreign-uid daemon entry, so a host trusty-search process no longer fails it with `InstanceUnreadable` (#11085). `restart` behaviour is unchanged.
