Added

- `GET /api/console/machine-status`: aggregated whole-machine status combining
  host resources (CPU, memory, disk, network) with a per-service health rollup.
  A background sampler (`host_status`) keeps the host snapshot warm; the route
  assembles `MachineStatus` from it plus the cached per-service reports. Data
  endpoint for the phase-2 Foundry dashboard (#6517).
