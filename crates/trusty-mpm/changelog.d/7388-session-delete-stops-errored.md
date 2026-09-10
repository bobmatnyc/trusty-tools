Fixed

- `tm session delete <id>` on an errored session now stops its runtime and then
  deletes the record, instead of exiting 1 with advice to run `tm session stop`
  first. The verb reads the record's state and routes through the same
  `route_delete_for_state` seam the `tm ls` picker uses, so the two surfaces
  cannot answer the same state differently (#7388). A running
  (`active`/`provisioning`) session still refuses without `--force`, and a stop
  the daemon rejects issues no delete.
