Changed

- **`main.rs` split: the `--legacy-in-process` run-task path moved to
  `cli::legacy_run_task` (issue #4434).** `main.rs` had reached 498 of the
  mechanically-enforced 500-SLOC production cap after #4424 added
  `Command::Tui`, so the next change to the file — any change — would have
  failed `scripts/check_line_cap.sh` before it started. The legacy in-process
  wrapper (`run_task`) and the two helpers only it uses
  (`validate_agent_name`, `build_llm_client`), together with their tests and
  the `TCODE_ENGINEER_MODEL` constant, now live in
  `crates/trusty-code/src/cli/legacy_run_task.rs` next to every other
  subcommand handler; `main.rs` is 335 SLOC of clap definitions plus
  dispatch. Behaviour-preserving: no test expectation changed, and the moved
  code is byte-identical apart from its module docs.
