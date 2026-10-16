Changed
- `tm services` probes the trusty-mpm daemon with `mpm.health` on its Unix socket (`health_probe: uds_mpm`), not `http://localhost:7880/health`; it reports no port and no URL for it. A `services.yaml` from an older `tm services init` that probes 7880 is mapped on read with one warning and is not rewritten (#6288).
