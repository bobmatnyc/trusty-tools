Fixed
- `IndexJournal::append` now reads only the journal's last byte to decide on a healing newline, so a feed pass no longer reads the whole journal once per record (O(N^2)) (#10875).
