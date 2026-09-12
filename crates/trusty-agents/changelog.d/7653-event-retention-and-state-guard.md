Fixed

- Retain published event knowledge when unrelated incoming records move it outside a bounded processing batch; withdraw only on explicit exclusion or revoked source access.
- Reject an oversized knowledge checkpoint before it replaces the file on disk, so the previous state stays readable and the next write recovers.
