Fixed

- **A journal torn mid-UTF-8-codepoint no longer bricks its source.**
  `Ledger::load` read the file with `read_to_string`, which fails outright on
  invalid UTF-8 — and a crash mid-write lands mid-codepoint whenever a record
  carries non-ASCII text (a Gmail subject with an em-dash, a Drive filename in
  any non-Latin script). The whole source then failed to load on every
  subsequent run, turning a one-item loss into permanent breakage. The parse is
  now byte-oriented: lines are split on `b'\n'` and decoded independently, so an
  undecodable line is counted as malformed exactly like invalid JSON.
- **Items with no revision signal are no longer frozen forever.** A constant
  fingerprint is indistinguishable from "unchanged", so an item whose source
  reports neither a version nor a modified time was skipped on every run after
  the first, silently serving stale content — the opposite of the intended
  fail-open. `SourceItem::volatile` marks such items and bypasses the skip test
  entirely.
