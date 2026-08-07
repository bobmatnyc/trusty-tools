Added

- `stats` JSON-RPC method, reporting `doc_count` and `total_text_bytes`. The
  protocol could say "here are your hits" but not "is there anything to hit",
  so a caller could not tell an indexed palace with no match from an unindexed
  one.
