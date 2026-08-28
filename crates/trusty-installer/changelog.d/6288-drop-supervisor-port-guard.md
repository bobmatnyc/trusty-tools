Changed
- Dropped `SUPERVISOR_METRICS_PORT`, the `LaunchctlPort::port_guard` seam, and the `TRUSTY_MPM_SUPERVISOR_ADDR` key from the supervisor plist template. The supervisor binds no port since #6288, so there is none for a foreign process to hold (Refs #6288).
