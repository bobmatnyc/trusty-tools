Changed
- The `tm supervisor` process no longer serves HTTP on `127.0.0.1:7881`. It publishes each sweep's snapshot to `~/.trusty-mpm/supervisor-metrics.json`, which the daemon reads (Refs #6288).
- `SupervisorConfig.metrics_addr`, `TRUSTY_MPM_SUPERVISOR_ADDR`, and `tm supervisor --addr` are removed; a leftover value in an operator's environment is ignored.
