//! `trusty-computer`: the broker and operator CLI (Phase 0 stub).
//!
//! Why: the bin exists from Phase 0 so packaging, the sandbox script and the
//! gates see the final layout (plan section 2).
//! What: prints usage and exits 2; the verbs arrive in Phases 3, 4 and 6.
//! Test: none yet; the CLI tests come with the verbs.

use std::process::ExitCode;

const USAGE: &str = "\
usage: trusty-computer <command>

commands (not implemented yet, issue #11460):
  serve --state DIR        run the broker
  grant <bundle-id>        mint a grant code for one app
  revoke <grant-id>        end a grant and its lease
  grants | status | stop   operator queries
  install-service          sign, write the launchd plist, bootstrap";

fn main() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
