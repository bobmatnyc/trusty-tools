Added

- `DELETE /indexes/{id}` and `search.index.delete` accept an optional `expected_root_path`. When present the delete is refused with `409` unless the registration's current root path is exactly that value, so a caller acting on a stale census cannot remove an index that was re-registered under the same id in the meantime. A registration that is absent, and a registry file that cannot be read, are refusals too — a comparison that could not run never permits the delete. Absent, the delete behaves exactly as before (#6380).
