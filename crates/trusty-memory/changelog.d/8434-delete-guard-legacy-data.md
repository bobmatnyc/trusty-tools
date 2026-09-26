Fixed

- `palace_delete` without `force` now refuses a palace whose live store is empty but whose legacy `kg.db` holds drawers the live store lacks, holds legacy triples, or cannot be read, or which still has a `.v2-incompatible` file. It also refuses a palace it cannot open or whose drawer table loaded degraded, instead of deleting it unchecked. Before, it deleted that data with no copy left. The `palace_delete` tool schema now states that `force` also destroys unimported `kg.db` data ([#8434](https://github.com/bobmatnyc/trusty-tools/issues/8434))
