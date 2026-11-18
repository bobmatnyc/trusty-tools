//! `trusty-browser`: the broker, the Chrome native host and the operator verbs.
//!
//! Why: one binary carries every role, so the enrollment pins can name a
//! single executable on each side (`peer_darwin`). The launchd job runs
//! `<exe> --state <dir> serve`; the Chrome launchers run
//! `<exe> native-host --socket <sock> <origin>`.
//! What: all parsing and dispatch live in [`trusty_browser::cli`].

fn main() -> std::process::ExitCode {
    trusty_browser::cli::main()
}
