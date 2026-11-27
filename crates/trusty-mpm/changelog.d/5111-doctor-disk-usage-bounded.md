Fixed

- `tm doctor`'s `disk_usage` row now lists mounts on the blocking pool with a 5 s deadline, so a slow mount listing no longer stalls the daemon's request worker; a missed deadline reports the row as undetermined (#5111).
