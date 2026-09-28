Fixed
- `kg-rebuild --purge-stale-subjects` and `--merge-punctuated-twins` now take the data root's maintenance lease before the rebuild step and hold it until both passes finish. While another process holds the lease, the command refuses, names the holder's pid, and writes nothing. A lease file that cannot be opened also refuses. `--dry-run` still needs no lease (#8744).
