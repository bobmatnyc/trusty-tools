Fixed
- The analyze dashboard's connection-error text no longer tells the operator to make trusty-search reachable on port 7878, which the daemon no longer binds; it points at `trusty-search status` (#9214).
- The console's port-collision guard no longer reserves 7878 for trusty-search (#9214).
