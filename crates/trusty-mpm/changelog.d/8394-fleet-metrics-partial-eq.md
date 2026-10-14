Changed
- `FleetMetrics`, `PublishedMetrics` and `SupervisorMetricsStatus` no longer implement `Eq`, because the new context figures are `f64`; `PartialEq` is unchanged (#8394).
