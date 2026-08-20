Documentation

- `doctor_worktrees.rs`'s `[`run_doctor`]` intra-doc link no longer breaks
  `cargo doc` for the workspace. `run_doctor` lives in the sibling module
  `daemon::doctor` and was never imported into `doctor_worktrees.rs`, so the
  bare link did not resolve; it now reads
  `[`crate::daemon::doctor::run_doctor`]`.
