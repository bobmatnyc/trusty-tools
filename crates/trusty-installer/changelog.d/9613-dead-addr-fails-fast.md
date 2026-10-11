Fixed
- The confirmed-down tests' `dead_addr` fixture now names a multicast address
  that the kernel refuses inside `connect(2)`, instead of `127.0.0.1:1`. Ten
  probe, verify and pinned-download tests no longer time out on a host that
  hangs a connect to a closed loopback port, such as WSL2 with mirrored
  networking (#9613).
