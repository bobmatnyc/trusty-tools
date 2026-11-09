Added
- `control_bus::METHOD_EVENTS_CONSOLE_METRICS` (`"events.console_metrics"`), `EventsConsoleMetricsParams`, and the reserved `by_project` keys `UNATTRIBUTED_PROJECT` and `OVERFLOW_PROJECT` (#10906).
- `console_metrics::by_convention`: the contract note for the `<svc>.console_metrics` UDS method and the `by_*` breakdown convention, with `check_by_convention` to test a payload against it (#10906).
- `HarnessSource::as_str` and `MessageDirection::as_str`, the wire strings without serialising (#10906).
