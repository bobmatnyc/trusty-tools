Fixed

- Two `cargo test -p trusty-mpm` cases no longer read whatever `$HOME` a sibling
  test happened to have set. `session_plan_under_matches_session_plan_at_home`
  resolved its two halves through two separate `$HOME` reads, and
  `rpc_tmux_snapshot_unknown_session_reports_a_coded_error` compared a refusal
  whose text quotes the live `$HOME`; a test moving `$HOME` between the reads
  failed either one while proving nothing about the code under test. Both now
  pin `$HOME` at a tempdir and join the crate-wide serial group (#6580).
