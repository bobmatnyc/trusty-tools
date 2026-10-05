Fixed
- The citation-integrity check reads a ranged or anchored `[code: …]` locator
  (`a.rs:10-20`, `a.rs:L10`, `a.rs:L10-L20`, `a.rs#L10-L20`) as its path and
  start line. A true finding quoting such a range is no longer withheld as
  "not part of the diff", and a range starting past the file's last diffed
  line is now withheld. `a.rs:10:5` reads as line 10, column 5 (#9188).
