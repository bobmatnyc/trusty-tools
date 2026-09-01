Added

- The Overview tab leads with a whole-machine status dashboard built to the
  Foundry design system: a four-card row for host CPU, memory, disk and network,
  each stamped with the pressure band the server classified, and a rollup card
  counting every reporting service with a per-service table of version, health
  and collection time. It polls `GET /api/console/machine-status` every 15s and
  says "first sample pending" while the host cache is still cold, rather than
  reporting HTTP 503. The existing per-service card grid stays below it — that
  grid is the only place a never-installed or absent service is visible (#6518).
