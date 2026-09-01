Added

- `host_metrics` module (behind the new `host-metrics` feature): whole-machine
  CPU, memory (with a provisional pressure signal), disk (per-mount + aggregate),
  and network-throughput sampling via `HostSampler`, plus the
  `console_metrics::machine_status::MachineStatus` aggregation that rolls up the
  per-service `ConsoleMetricsReport`s. Data foundation for the Foundry
  machine-status dashboard (#6517). The `host-metrics` feature enables `sysinfo`'s
  `disk`/`network` features only for opt-in consumers.
