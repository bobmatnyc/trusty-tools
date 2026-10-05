Fixed
- The citation-integrity check reads a ranged or anchored `[code: …]` locator
  (`a.rs:10-20`, `a.rs:L10`, `a.rs:L10-L20`, `a.rs#L10-L20`, `a.rs:10:5`) as
  its path and start line, so a range starting past the file's last diffed
  line is withheld. A true finding that quotes a `:` range form (`:N-M`,
  `:LN`, `:LN-LM`) is no longer withheld as "not part of the diff" (#9188).
