Changed
- The console's `known_siblings` port guard keeps 7880 as a collision-history row for the retired `trusty-mpm` daemon port and no longer points at `DEFAULT_DAEMON_ADDR`, which #6288 S3a removed with the daemon's TCP bind (#6288).
