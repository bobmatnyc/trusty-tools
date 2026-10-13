Fixed
- An owner-only `AuditLog` append now holds an exclusive file lock from the length read through the write, the fsync and any cut-back. A failed append no longer truncates away a record another writer appended to the same file in between. When the file ends in a torn line, the next owner-only append starts on a line of its own instead of joining the fragment (Refs #8433).
