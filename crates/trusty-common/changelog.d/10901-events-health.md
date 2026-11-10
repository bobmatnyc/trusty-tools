Added
- `control_bus::METHOD_EVENTS_HEALTH` (`"events.health"`) with its `EventsHealthParams` request and `EventsHealth` reply (`status`, `service`, `version`, `uptime_secs`, `ingested`, `deduped`, `evicted`, `log_dropped`, `subscribers`, `head_seq`), the health half of the trusty-events socket contract (#10901).
