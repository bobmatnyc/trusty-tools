//! `trusty-computer`: the broker and operator CLI.
//!
//! Why: the bin exists from Phase 0 so packaging, the sandbox script and the
//! gates see the final layout (plan section 2).
//! What: `serve --state DIR` runs the supervised bounded daemon (Phase 2 form,
//! see `trusty_computer::serve`); every other verb prints usage and exits 2
//! until Phases 3, 4 and 6.
//! Test: `live_bounded_daemon_drives_fixture` in `tests/live_supervisor.rs`.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
usage: trusty-computer <command>

commands (issue #11460):
  serve --state DIR        run the supervised bounded daemon (Phase 2 form)
  grant <bundle-id>        mint a grant code for one app (not implemented yet)
  revoke <grant-id>        end a grant and its lease (not implemented yet)
  grants | status | stop   operator queries (not implemented yet)
  install-service          sign, write the launchd plist, bootstrap (not implemented yet)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [serve, flag, dir] if serve == "serve" && flag == "--state" => {
            ExitCode::from(trusty_computer::serve::run(Path::new(dir)))
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
