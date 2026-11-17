//! `tm architect [up|attach]`: the CLI half of #11364.
//!
//! Why: bare `tm architect` must reach the Architect from any prompt (owner
//! ruling Dpg), so the directory and session come from the recorded config,
//! never the current directory.
//! What: [`up`] resolves the directory ([`fleet::resolve_dir`]) and the
//! recorded session names, runs [`fleet::up::up`], sends the daemon
//! registration, prints the report and attaches unless told not to or run
//! inside Claude Code, where it prints the attach command instead.
//! Test: `tests/tm_architect_up.rs`; the logic in `fleet/up_tests.rs`.

use crate::cli::UpArgs;
use crate::commands::fleet::{self, up as fleet_up};

/// `tm architect up`; returns the exit code.
pub(crate) async fn up(args: UpArgs) -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("tm architect: cannot resolve the home directory");
        return 1;
    };
    let dir = match fleet::resolve_dir(args.dir.as_deref(), &home) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tm architect: {e:#}");
            return 2;
        }
    };
    let names = match fleet::recorded_names(&home) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("tm architect: {e}");
            return 2;
        }
    };
    let inside = fleet::inside_claude_code();
    let mut report = fleet_up::up(&dir, &home, &names, inside, args.dry_run, fleet_up::LIVE);
    if let Some(reg) = report.registration.take() {
        let step = fleet::register_architect(&reg).await;
        report.steps.push(step.render_line().trim().to_owned());
    }
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(j) => println!("{j}"),
            Err(e) => eprintln!("tm architect: {e}"),
        }
    } else {
        print!("{}", report.render());
    }
    if report.exit != 0 || args.no_attach || args.json || args.dry_run {
        return report.exit;
    }
    attach_to(names.architect(), inside)
}

/// `tm architect attach`; returns the exit code.
pub(crate) fn attach() -> i32 {
    let names = match dirs::home_dir().map(|h| fleet::recorded_names(&h)) {
        Some(Ok(n)) => n,
        Some(Err(e)) => {
            eprintln!("tm architect: {e}");
            return 2;
        }
        None => {
            eprintln!("tm architect: cannot resolve the home directory");
            return 1;
        }
    };
    attach_to(names.architect(), fleet::inside_claude_code())
}

/// `tm architect restart` (#11365); returns the exit code (0, 1, 2 or 4).
pub(crate) async fn restart(wait: Option<u64>, dir: Option<String>) -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("tm architect: cannot resolve the home directory");
        return 1;
    };
    let resolved = fleet::resolve_dir(dir.as_deref(), &home).and_then(|d| {
        let names = fleet::recorded_names(&home).map_err(anyhow::Error::msg)?;
        Ok((std::fs::canonicalize(&d).unwrap_or(d), names))
    });
    let (dir, names) = match resolved {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tm architect: {e:#}");
            return 2;
        }
    };
    let wait = wait.map(std::time::Duration::from_secs);
    let inside = fleet::inside_claude_code();
    let mut report = fleet::restart::restart_live(&dir, &home, &names, inside, wait);
    if let Some(reg) = report.registration.take() {
        let step = fleet::register_architect(&reg).await;
        report.lines.push(step.render_line().trim().to_owned());
    }
    for line in &report.lines {
        println!("  {line}");
    }
    println!("tm architect restart: exit {}", report.exit);
    report.exit
}

/// Attach (or switch-client inside tmux); inside Claude Code only print the command.
fn attach_to(session: &str, inside: bool) -> i32 {
    if inside {
        println!("attach from a terminal: tmux attach -t ={session}:");
        return 0;
    }
    match crate::commands::tmux_attach::tmux_attach(session) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("tm architect: {e:#}");
            1
        }
    }
}
