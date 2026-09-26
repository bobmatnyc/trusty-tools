Fixed

- `palace_delete` without `force` now refuses a palace whose live store is empty but whose legacy `kg.db` holds drawers the live store lacks, or which still has a `.v2-incompatible` file. Before, it deleted that data with no copy left ([#8434](https://github.com/bobmatnyc/trusty-tools/issues/8434))
