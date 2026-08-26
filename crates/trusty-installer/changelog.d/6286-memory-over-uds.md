Changed
- `tctl` probes trusty-memory over its Unix socket (`memory.health`) rather than dialling `7070` (#6286, ADR-0032). `fixed_port_for("trusty-memory")` is gone: leaving it would make every `tctl install` dial a port the daemon no longer binds, read the refusal as confirmed-down, and `launchctl kickstart -k` a healthy daemon — #4246 verbatim
- The port guard resolves NO port for trusty-memory, including when a pre-migration `http_addr` is still on disk. That file is deleted by the daemon at every start, but only once it has started, and the guard runs before that
