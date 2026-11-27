Changed
- The `tm fleet`, `tm doctor` build-lease, guided-start, `tm launch --twin`, `tm watch` and `tm issue` messages that name a home path for the reader carry a `path-literal: allow` waiver, and their tests name the `.trusty-mpm`, `.trusty-tools` and `trusty-mpm-projects` dirs through the shared trusty-common constants; every message is byte-identical (#11821).
