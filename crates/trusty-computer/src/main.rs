//! `trusty-computer`: the broker and operator CLI.
//!
//! Why: the bin exists from Phase 0 so packaging, the sandbox script and the
//! gates see the final layout (plan section 2).
//! What: `serve --state DIR` runs the supervised bounded daemon (Phase 2 form,
//! see `trusty_computer::serve`); `broker --state DIR` runs the broker on
//! `agent.sock` and `operator.sock` until SIGTERM; every other verb prints
//! usage and exits 2 until Phases 4 and 6.
//! Test: `live_bounded_daemon_drives_fixture` in `tests/live_supervisor.rs`.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
usage: trusty-computer <command>

commands (issue #11460):
  serve --state DIR        run the supervised bounded daemon (Phase 2 form)
  broker --state DIR       run the broker (agent.sock, operator.sock)
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
        [verb, flag, dir] if verb == "broker" && flag == "--state" => broker(Path::new(dir)),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

static TERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Run the broker until SIGTERM or SIGINT, then stop its daemon.
fn broker(state: &Path) -> ExitCode {
    use std::sync::Arc;
    use trusty_computer::{broker, broker_server, launcher, supervisor};
    let started = supervisor::Supervisor::new(launcher::CuaAppLauncher, state)
        .map_err(|e| e.to_string())
        .and_then(|sup| {
            broker::Broker::new(sup, state, broker::wall_clock()).map_err(|e| e.to_string())
        })
        .and_then(|b| {
            broker_server::BrokerServer::bind(state)
                .map(|s| (Arc::new(b), s))
                .map_err(|e| e.to_string())
        });
    let (broker, mut server) = match started {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("trusty-computer broker: {e}");
            return ExitCode::from(1);
        }
    };
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    server.spawn(Arc::clone(&broker));
    println!(
        "ready agent={} operator={}",
        server.agent.display(),
        server.operator.display()
    );
    while !TERM.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = broker.lock().supervisor.stop();
    ExitCode::SUCCESS
}
