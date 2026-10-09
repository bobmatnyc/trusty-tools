Changed
- `trusty-search monitor` reads the daemon through the socket-only trusty-common `SearchClient`; an unreachable daemon is reported with the socket path (#9214).
