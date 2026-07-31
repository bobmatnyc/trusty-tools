//! `tcode` CLI subcommand handlers (#2060) — the binary-side half of the
//! thin-client architecture whose library half is `trusty_code::cli_client`.
//!
//! Why: kept out of `main.rs` (which would otherwise grow past the 500-SLOC
//! production cap once every subcommand's handler landed) so each
//! subcommand's wiring — argument-shaped setup, spawning a client, calling
//! the daemon, printing, picking an exit code — lives in its own small,
//! focused file. None of these files contain business logic: every decision
//! (session lifecycle, task execution, transcript contents) is made by the
//! daemon; these handlers only translate CLI args into JSON-RPC calls and
//! JSON-RPC results into terminal output, exactly matching the vision
//! spec's Axiom 3 (API-First) / §4.4 (CLI as Thin Client) requirement.
//! What: [`tcode_exe::resolve`] (locate the running binary so it can
//! re-spawn itself as `tcode serve --stdio`), [`session::list`]/
//! [`session::status`], [`run_task::run`] (create + `task.run` + attach +
//! stream — the M1 cut-line "replay via thin CLI" verb), [`attach::run`],
//! [`cancel::run`], [`transcript::run`], and (#3296)
//! [`workstream::list`]/[`workstream::get`]/[`workstream::create`]/
//! [`workstream::activate`]/[`workstream::deactivate`]/[`workstream::close`].
//! (#4424) [`tui::run`] is one of two members that are NOT
//! JSON-RPC-over-stdio translators: it is the launch point for the
//! interactive TUI REPL, which talks to a long-lived `tcode serve --http`
//! daemon instead. It lives here anyway because it is the same KIND of thing
//! — argument-shaped setup plus a handoff, with every decision made
//! elsewhere (see its module docs). (#4434) [`legacy_run_task::run`] is the
//! other: it is `run-task --legacy-in-process`, the pre-#2060 path that runs
//! the `AgentLoop` in THIS process rather than driving a daemon, so it alone
//! carries CLI-shaped setup no thin client needs (agent-name validation,
//! engineer-model resolution, LLM-client construction). It is a subcommand
//! handler like every other file here, and was moved out of `main.rs` when
//! that file reached 498 of the 500-SLOC cap; see its module docs.
//! Test: each submodule's own doc comments note its coverage; the full
//! spawn+wire behaviour is covered end-to-end by `tests/cli_e2e.rs` against
//! the real `tcode` binary — these handlers are too thin to usefully unit
//! test in isolation (they are a few lines of glue over
//! `trusty_code::cli_client`, which carries its own unit tests).

pub mod attach;
pub mod cancel;
pub mod legacy_run_task;
pub mod run_task;
pub mod session;
pub mod tcode_exe;
pub mod transcript;
pub mod tui;
pub mod workstream;
