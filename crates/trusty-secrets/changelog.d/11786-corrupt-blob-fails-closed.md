Security
- A corrupt or tampered vault blob now fails every read with a fixed error that names the `tm secrets migrate --rollback` recovery. A read no longer falls back to the migration backup or the per-item items, which could serve a key rotated or deleted after migration (#11786).
