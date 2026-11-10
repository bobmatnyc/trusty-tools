Changed
- `tm-fleet-check` step 1 runs `scripts/watch-apply.py` before `tm fleet project list`, so the owner's dashboard Watch and Unwatch requests are applied through the CLI at the start of each pass; a `failed` request is a finding (#10896).
- `architect-dashboard` lists the `watch_changes` collection and its `watch_change` owner event, and says to apply it only through `watch-apply.py`, never by editing `local/watch.json` (#10896).
