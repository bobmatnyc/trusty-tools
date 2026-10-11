Fixed
- Three `pm_guard_dispatch` no-daemon tests now dial a `dead_addr` fixture, a multicast address the kernel refuses inside `connect(2)`, instead of `127.0.0.1:1`. They no longer fail on a host that hangs a connect to a closed loopback port past the guard's 500 ms connect timeout, such as WSL2 with mirrored networking (#9695).
