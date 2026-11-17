Added
- `events.console_metrics` on the bus socket: a `ConsoleMetricsReport` whose `metrics` counts accepted events by source and payload domain (`by_source`), `message` events by direction (`messages`: `sent`, `received`), and both again per project (`by_project`). An event with no project counts as `unattributed`; past 256 distinct projects a new one counts as `overflow` (#10906).
