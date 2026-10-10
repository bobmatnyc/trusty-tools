Fixed
- `secrets.resolve` writes its allow audit record before it opens or reads the backend. When the audit log cannot be opened or written, the call answers `audit_unavailable` and reads nothing, so no value is read without a record. A failure after the record (the agents flag, the backend, a spent grant) adds a deny record with its kind (#9621).
