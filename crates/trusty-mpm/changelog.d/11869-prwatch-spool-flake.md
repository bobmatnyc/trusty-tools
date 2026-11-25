Fixed
- The PR watch test `a_publisherless_emit_lands_in_the_spool` no longer reads 0 spooled events when a parallel test spawns a child that briefly holds the spool file lock; it relinks the spool file before the take, as the #10748 tests do (Refs #11869)
