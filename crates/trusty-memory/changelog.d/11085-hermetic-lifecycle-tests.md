Changed
- The Linux lifecycle restart tests scan an injected process table of their own processes, so a host trusty-memory daemon no longer changes their result (#11085). `restart` behaviour is unchanged.
