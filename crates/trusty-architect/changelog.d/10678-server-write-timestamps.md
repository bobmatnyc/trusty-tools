Fixed
- The dashboard store stamps `created` and `updated` from its own clock on every write and ignores a client value for either; `created` keeps the stored value, and one batch uses one write time for every document and its change-log line (#10678).
