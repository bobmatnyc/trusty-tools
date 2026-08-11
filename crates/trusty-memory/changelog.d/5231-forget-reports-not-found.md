Fixed

- `memory_forget` returns `status: "not_found"` for a drawer id that does not exist, instead of reporting `status: "deleted"` for a delete that never happened, and no longer emits a `DrawerDeleted` activity event for it. A cleanup loop could report N deletions having made zero (closes [#5231](https://github.com/bobmatnyc/trusty-tools/issues/5231))
  - `DELETE /api/v1/palaces/{id}/drawers/{drawer_id}` answers `404` for an unknown drawer id instead of `204 No Content`, matching the `delete_palace` behaviour established in #180. A malformed id is still a `400`
