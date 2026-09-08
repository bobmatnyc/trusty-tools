Added

- `residency` module (unconditional): `ActiveProjectSet`/`ActiveProject` wire types and `ResidencySnapshot`, the injected-clock freshness state machine every active-project-residency consumer wraps a pulled set in, plus the four `TRUSTY_RESIDENCY_*` env parsers (#7087 slice 1a).
- `mpm_rpc` module (behind the `uds` feature): `fetch_active_projects`/`fetch_active_projects_at` client for the trusty-mpm daemon's forthcoming `mpm.residency.active` method (#7087 slice 1a).
