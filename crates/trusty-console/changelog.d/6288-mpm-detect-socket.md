Changed
- The trusty-mpm service card is detected with `mpm.health` on the daemon's Unix socket (honouring `TRUSTY_MPM_SOCKET`), not its `http_addr` file and a TCP probe; it carries no URL. An error answer reads `Degraded` with the daemon's error as the hint (#6288).
- The trusty-mpm CPU and memory graphs find the daemon's pid from its socket peer, not the lock file (#6288).
