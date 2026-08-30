Changed

- Deleting a search index from the dashboard now deletes its on-disk data by default, and keeping the data is the explicit opt-out (owner ruling, #6422).
  - The per-row confirm still says "This cannot be undone." and still needs a second click; what changed is that its `delete_data` checkbox starts TICKED, labelled "Delete the on-disk data too — untick to deregister only and keep the corpus". A palace delete is unaffected: `force` widens what a delete may destroy and stays opt-in, so its box still starts unticked.
  - The stale-registration prune panel starts the same way. Its confirm sentence already named the fate of the data either way; now the default it names is deletion.
  - `DELETE /api/console/search/indexes/{id}` and `POST /api/console/search/prune-indexes` both read an absent `delete_data` as `true`. The UI sends the value explicitly regardless.
