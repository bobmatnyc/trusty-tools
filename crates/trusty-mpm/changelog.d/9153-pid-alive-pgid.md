Fixed
- A daemon lock naming pid 0 or a pid above `i32::MAX` now reads as dead. `pid_alive` used to cast the pid to `pid_t`, so `kill` probed a process group instead of one process and the corrupt lock read as alive (#9153).
- `tm slack stop` refuses a PID file naming pid 0 or a pid above `i32::MAX` instead of sending `SIGTERM` to a whole process group, which for pid 0 was its own (#9153).
