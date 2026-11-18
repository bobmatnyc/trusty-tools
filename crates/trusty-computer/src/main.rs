//! `trusty-computer`: the broker and operator CLI.
//!
//! Why: the bin exists from Phase 0 so packaging, the sandbox script and the
//! gates see the final layout (plan section 2).
//! What: `serve --state DIR` runs the supervised bounded daemon (Phase 2 form,
//! see `trusty_computer::serve`); `broker --state DIR` runs the broker on
//! `agent.sock` and `operator.sock` until SIGTERM or a `lockout`, with the
//! real `osascript` consent dialog; `grant`, `grants`, `revoke`, `status`,
//! `lease`, `lockout` and `unlock` are the operator verbs (`operator_cli`);
//! `install-service` and `uninstall-service` manage the launchd job (`service_cli`).
//! Test: `live_bounded_daemon_drives_fixture`, `no_tty_grant_never_reaches_the_broker`.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
usage: trusty-computer <command>

commands (issue #11460):
  serve --state DIR        run the supervised bounded daemon (Phase 2 form)
  broker --state DIR       run the broker (agent.sock, operator.sock)
  grant <bundle-id> --pid N --window N [--ttl 30m] [--idle-ttl 5m] [--tools a,b] [--allow-sensitive]
                           confirm in the dialog, type its code, get a one-use tc- code
  revoke <grant-id>        end a grant and its lease
  grants | status | lease  operator queries
  lockout                  revoke every grant, drop the lease, stop the daemon and broker;
                           persists across restarts until `unlock`
  unlock                   clear a lockout: confirm in the dialog, type its code
  install-service [--state DIR] [--sign-identity ID | --no-sign]
                           sign both bins, write the com.trusty.computer plist, bootstrap
  uninstall-service [--state DIR]
                           bootout the job and remove its plist
every operator verb takes --state DIR (default ~/.trusty/computer)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [serve, flag, dir] if serve == "serve" && flag == "--state" => {
            ExitCode::from(trusty_computer::serve::run(Path::new(dir)))
        }
        [verb, flag, dir] if verb == "broker" && flag == "--state" => broker(Path::new(dir)),
        [verb, rest @ ..] if verb == "install-service" || verb == "uninstall-service" => {
            ExitCode::from(trusty_computer::service_cli::run(
                verb == "install-service",
                rest,
            ))
        }
        [verb, ..]
            if [
                "grant", "grants", "revoke", "status", "lease", "lockout", "unlock", "--state",
            ]
            .contains(&verb.as_str()) =>
        {
            match trusty_computer::operator_cli::run(&args) {
                2 => {
                    eprintln!("{USAGE}");
                    ExitCode::from(2)
                }
                code => ExitCode::from(code),
            }
        }
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
            let dialog = Arc::new(trusty_computer::consent::OsascriptDialog);
            broker::Broker::new(sup, state, broker::wall_clock(), dialog).map_err(|e| e.to_string())
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
    while !TERM.load(std::sync::atomic::Ordering::SeqCst) && !broker.shutdown_requested() {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = broker.lock().supervisor.stop();
    ExitCode::SUCCESS
}
